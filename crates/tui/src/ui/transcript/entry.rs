//! Transcript entry metadata and mutable cell wrapper.

use std::sync::Arc;

use crate::ui::cell::{TextCell, ToolCallCell};
use crate::ui::transcript::cell::TranscriptCell;

/// Stable internal id for transcript entry cache keys.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TranscriptEntryId(u64);

impl TranscriptEntryId {
    /// Creates a new stable entry id.
    pub fn new(value: u64) -> Self {
        Self(value)
    }
}

/// Mutability state for one transcript entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TranscriptEntryState {
    /// Entry is stable and should only rerender on width/theme changes.
    Committed,
    /// Entry may still receive streaming updates.
    Active,
}

/// One transcript cell plus cache metadata.
#[derive(Debug, Clone, typed_builder::TypedBuilder)]
pub struct TranscriptEntry {
    /// Stable cache id for this entry.
    id: TranscriptEntryId,
    /// Render revision bumped when this entry's cell changes.
    #[builder(default)]
    revision: u64,
    /// Whether this entry can still receive streaming changes.
    state: TranscriptEntryState,
    /// Renderable cell stored behind a cheap cloneable pointer.
    cell: Arc<dyn TranscriptCell>,
}

impl TranscriptEntry {
    /// Creates a transcript entry around one cell.
    pub fn new(
        id: TranscriptEntryId,
        state: TranscriptEntryState,
        cell: Arc<dyn TranscriptCell>,
    ) -> Self {
        Self::builder().id(id).state(state).cell(cell).build()
    }

    /// Returns the stable entry id.
    pub fn id(&self) -> TranscriptEntryId {
        self.id
    }

    /// Returns the render revision for this entry.
    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// Returns whether this entry is active or committed.
    pub fn state(&self) -> TranscriptEntryState {
        self.state
    }

    /// Returns the underlying cell.
    pub fn cell(&self) -> Arc<dyn TranscriptCell> {
        Arc::clone(&self.cell)
    }

    /// Replaces the underlying cell and bumps this entry revision.
    pub fn replace_cell(&mut self, cell: Arc<dyn TranscriptCell>) {
        self.cell = cell;
        self.bump_revision();
    }

    /// Marks this entry as committed.
    pub fn commit(&mut self) {
        self.state = TranscriptEntryState::Committed;
    }

    /// Marks this entry as active.
    pub fn activate(&mut self) {
        self.state = TranscriptEntryState::Active;
    }

    /// Bumps this entry revision after mutating cell contents.
    pub fn bump_revision(&mut self) {
        self.revision = self.revision.wrapping_add(1);
    }

    /// Returns this entry as a text cell when possible.
    pub fn text_cell(&self) -> Option<&TextCell> {
        self.cell.as_ref().as_any().downcast_ref::<TextCell>()
    }

    /// Returns this entry as a tool-call cell when possible.
    pub fn tool_call(&self) -> Option<&ToolCallCell> {
        self.cell.as_ref().as_any().downcast_ref::<ToolCallCell>()
    }
}
