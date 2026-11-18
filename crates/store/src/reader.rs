//! Read-only decoding of v4 JSONL session logs for external projections.
//!
//! The reader decodes complete lines from any line offset without replaying
//! them, so an index can resume where it stopped and leave a torn tail for a
//! later read. Validation stays with [`crate::JsonlStoreFactory::open`].

use std::path::PathBuf;

use protocol::{
    EntryId, LaneId, Sequence, SessionGitInfo, SessionId, SessionOwnership,
    TimestampMs,
};

use crate::factory::{HeaderKind, V4Header};
use crate::state::{EntryMutation, LaneMutation, RecordMutation};
use crate::{SessionEntry, SessionRecord, StoreError};

/// Public read-only view of one v4 JSONL header line.
#[derive(Debug, Clone, PartialEq, Eq, typed_builder::TypedBuilder)]
pub struct LogHeader {
    /// Stable Session identifier.
    pub id: SessionId,
    /// Creation time recorded by the factory.
    pub created_at: TimestampMs,
    /// Working directory recorded at creation.
    pub cwd: PathBuf,
    /// Source Session when this Session was forked.
    #[builder(default)]
    pub parent_session_id: Option<SessionId>,
    /// Owner edge of a delegated Session.
    #[builder(default)]
    pub ownership: Option<SessionOwnership>,
    /// Git state captured at creation.
    #[builder(default)]
    pub git: Option<SessionGitInfo>,
    /// Product version that created the Session.
    #[builder(default)]
    pub app_version: Option<String>,
}

impl TryFrom<V4Header> for LogHeader {
    type Error = StoreError;
    /// Drops the format discriminants that a validated header no longer needs.
    fn try_from(header: V4Header) -> Result<Self, Self::Error> {
        if header.kind != HeaderKind::Header || header.version != 4 {
            return Err(StoreError::InvalidSession(
                "unsupported or missing v4 header".to_owned(),
            ));
        }
        Ok(Self::builder()
            .id(header.id)
            .created_at(header.created_at)
            .cwd(header.cwd)
            .parent_session_id(header.parent_session_id)
            .ownership(header.ownership)
            .git(header.git)
            .app_version(header.app_version)
            .build())
    }
}

impl TryFrom<&[u8]> for LogHeader {
    type Error = StoreError;

    /// Decodes one header line and rejects any other kind or format version.
    fn try_from(line: &[u8]) -> Result<Self, Self::Error> {
        let header: V4Header = serde_json::from_slice(line)
            .map_err(|error| StoreError::InvalidSession(error.to_string()))?;
        header.try_into()
    }
}

/// One decoded JSONL line, header included.
#[derive(Debug, Clone, PartialEq)]
pub enum LogMutation {
    /// The immutable first line of every log.
    Header(LogHeader),
    /// Lane creation or cursor movement.
    Lane {
        /// Shared log sequence.
        sequence: Sequence,
        /// Lane being created or moved.
        lane: LaneId,
        /// New lane leaf, or the root when absent.
        leaf_id: Option<EntryId>,
    },
    /// Transcript entry appended to a lane.
    Entry {
        /// Lane the entry was appended to.
        lane: LaneId,
        /// Complete stored entry.
        entry: SessionEntry,
    },
    /// Durable operation record.
    Record(SessionRecord),
    /// Global name fact; `None` clears the name.
    Name {
        /// Shared log sequence.
        sequence: Sequence,
        /// New Session name.
        name: Option<String>,
    },
    /// Label fact for one entry; `None` clears the label.
    Label {
        /// Shared log sequence.
        sequence: Sequence,
        /// Labelled entry.
        target_id: EntryId,
        /// New label.
        label: Option<String>,
    },
}

impl LogMutation {
    /// Reads the name fact without decoding unrelated entries during a fallback listing.
    pub(crate) fn name_fact(value: &serde_json::Value) -> Option<Option<&str>> {
        (value.get("kind")?.as_str()? == "fact"
            && value.get("fact")?.as_str()? == "name")
            .then(|| value.get("name").and_then(serde_json::Value::as_str))
    }

    /// Returns the shared log sequence, or `None` for the header.
    #[must_use]
    pub fn sequence(&self) -> Option<u64> {
        match self {
            Self::Header(_) => None,
            Self::Lane { sequence, .. }
            | Self::Name { sequence, .. }
            | Self::Label { sequence, .. } => Some(sequence.get()),
            Self::Entry { entry, .. } => Some(entry.sequence.get()),
            Self::Record(record) => Some(record.sequence.get()),
        }
    }
}

impl TryFrom<&[u8]> for LogMutation {
    type Error = StoreError;

    /// Decodes one complete line by its `kind` and, for facts, its `fact` tag.
    fn try_from(line: &[u8]) -> Result<Self, Self::Error> {
        let value: serde_json::Value = serde_json::from_slice(line)?;
        Self::try_from(value)
    }
}

impl TryFrom<serde_json::Value> for LogMutation {
    type Error = StoreError;

    /// Decodes one owned JSON object with the same fact semantics used by replay and listing.
    fn try_from(value: serde_json::Value) -> Result<Self, Self::Error> {
        let tag =
            |key: &str| value.get(key).and_then(serde_json::Value::as_str);
        let sequence = || {
            value
                .get("seq")
                .and_then(serde_json::Value::as_u64)
                .and_then(|seq| Sequence::try_from(seq).ok())
                .ok_or_else(|| {
                    StoreError::InvalidSession(
                        "mutation has invalid sequence".to_owned(),
                    )
                })
        };
        if let Some(name) = Self::name_fact(&value) {
            return Ok(Self::Name {
                sequence: sequence()?,
                name: name.map(ToOwned::to_owned),
            });
        }
        match (tag("kind"), tag("fact")) {
            (Some("header"), _) => Ok(Self::Header(
                serde_json::from_value::<V4Header>(value)?.try_into()?,
            )),
            (Some("lane"), _) => {
                let mutation: LaneMutation = serde_json::from_value(value)?;
                Ok(Self::Lane {
                    sequence: mutation.sequence,
                    lane: mutation.lane,
                    leaf_id: mutation.leaf_id,
                })
            }
            (Some("entry"), _) => {
                let mutation: EntryMutation = serde_json::from_value(value)?;
                Ok(Self::Entry {
                    lane: mutation.lane,
                    entry: mutation.entry,
                })
            }
            (Some("record"), _) => {
                let mutation: RecordMutation = serde_json::from_value(value)?;
                Ok(Self::Record(mutation.record))
            }
            (Some("fact"), Some("label")) => {
                let target = tag("targetId").ok_or_else(|| {
                    StoreError::InvalidSession(
                        "label fact has no targetId".to_owned(),
                    )
                })?;
                Ok(Self::Label {
                    sequence: sequence()?,
                    target_id: EntryId::try_from(target).map_err(|error| {
                        StoreError::InvalidSession(error.to_string())
                    })?,
                    label: tag("label").map(ToOwned::to_owned),
                })
            }
            (kind, fact) => Err(StoreError::InvalidSession(format!(
                "unknown mutation kind {kind:?} fact {fact:?}"
            ))),
        }
    }
}

/// One decoded line and the absolute byte range it occupied.
#[derive(Debug, Clone, PartialEq)]
pub struct LoggedMutation {
    /// Absolute offset of the line's first byte.
    pub offset: u64,
    /// Absolute offset just past the line's newline.
    pub next_offset: u64,
    /// Decoded line content.
    pub mutation: LogMutation,
}

/// Iterates complete lines of a log slice that starts at `base_offset`.
pub struct MutationReader<'a> {
    bytes: &'a [u8],
    base_offset: u64,
    cursor: usize,
}

impl<'a> MutationReader<'a> {
    /// Reads `bytes`, which begin at absolute file offset `base_offset`.
    #[must_use]
    pub fn new(bytes: &'a [u8], base_offset: u64) -> Self {
        Self {
            bytes,
            base_offset,
            cursor: 0,
        }
    }

    /// Converts a slice position into an absolute file offset.
    fn absolute(&self, position: usize) -> u64 {
        self.base_offset
            .saturating_add(u64::try_from(position).unwrap_or(u64::MAX))
    }
}

impl Iterator for MutationReader<'_> {
    type Item = Result<LoggedMutation, StoreError>;

    /// Yields the next newline-terminated line; a torn tail ends iteration.
    fn next(&mut self) -> Option<Self::Item> {
        let rest = self.bytes.get(self.cursor..)?;
        let length = rest.iter().position(|byte| *byte == b'\n')?;
        let line = rest.get(..length)?;
        let offset = self.absolute(self.cursor);
        self.cursor = self.cursor.saturating_add(length).saturating_add(1);
        let next_offset = self.absolute(self.cursor);
        Some(LogMutation::try_from(line).map(|mutation| LoggedMutation {
            offset,
            next_offset,
            mutation,
        }))
    }
}
