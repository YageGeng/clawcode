use std::collections::{HashMap, HashSet};

use protocol::{EntryId, LaneId, RecordId, Sequence};
use serde::{Deserialize, Serialize};

use crate::{SessionEntry, SessionRecord, StoreError};

/// Complete in-memory projection rebuilt from ordered JSONL mutations.
#[derive(Clone, typed_builder::TypedBuilder)]
pub(super) struct SessionState {
    pub(super) next_sequence: u64,
    pub(super) lanes: HashMap<LaneId, Option<EntryId>>,
    pub(super) entries: HashMap<EntryId, SessionEntry>,
    pub(super) entry_order: Vec<EntryId>,
    pub(super) records: Vec<SessionRecord>,
    pub(super) used_record_ids: HashSet<RecordId>,
    #[builder(default)]
    pub(super) name: Option<String>,
    pub(super) labels: HashMap<EntryId, String>,
}

impl SessionState {
    /// Creates pi's implicit root `main` lane and empty append-only state.
    pub(super) fn new() -> Result<Self, StoreError> {
        let main = LaneId::try_from("main")
            .map_err(|error| StoreError::InvalidSession(error.to_string()))?;
        Ok(Self::builder()
            .next_sequence(0)
            .lanes(HashMap::from([(main, None)]))
            .entries(HashMap::new())
            .entry_order(Vec::new())
            .records(Vec::new())
            .used_record_ids(HashSet::new())
            .labels(HashMap::new())
            .build())
    }
}

/// Stable JSONL mutation discriminants.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum MutationKind {
    Entry,
    Lane,
    Record,
    Fact,
}

/// Persisted lane creation or cursor movement.
#[derive(Serialize, Deserialize, typed_builder::TypedBuilder)]
#[serde(rename_all = "camelCase")]
pub(super) struct LaneMutation {
    pub(super) kind: MutationKind,
    #[serde(rename = "seq")]
    pub(super) sequence: Sequence,
    pub(super) lane: LaneId,
    #[builder(default)]
    pub(super) leaf_id: Option<EntryId>,
}

/// Persisted transcript entry and owning lane.
#[derive(Serialize, Deserialize)]
pub(super) struct EntryMutation {
    pub(super) kind: MutationKind,
    pub(super) lane: LaneId,
    #[serde(flatten)]
    pub(super) entry: SessionEntry,
}

/// Persisted operation record wrapper.
#[derive(Serialize, Deserialize)]
pub(super) struct RecordMutation {
    pub(super) kind: MutationKind,
    #[serde(flatten)]
    pub(super) record: SessionRecord,
}

/// Stable global fact discriminants.
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum FactKind {
    Name,
    Label,
}

/// Persisted global session-name fact.
#[derive(Serialize, typed_builder::TypedBuilder)]
pub(super) struct NameFactMutation {
    pub(super) kind: MutationKind,
    #[serde(rename = "seq")]
    pub(super) sequence: Sequence,
    pub(super) fact: FactKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[builder(default)]
    pub(super) name: Option<String>,
}

/// Persisted optional label for one transcript entry.
#[derive(Serialize, typed_builder::TypedBuilder)]
#[serde(rename_all = "camelCase")]
pub(super) struct LabelFactMutation {
    pub(super) kind: MutationKind,
    #[serde(rename = "seq")]
    pub(super) sequence: Sequence,
    pub(super) fact: FactKind,
    pub(super) target_id: EntryId,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[builder(default)]
    pub(super) label: Option<String>,
}
