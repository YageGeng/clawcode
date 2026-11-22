//! The right panel (M3b spec §6): each session's own tabs — changes, files,
//! file previews, and subagents — beside the main card, or as a sheet over
//! it when the window is too narrow.
//!
//! What a panel shows is saved per owner in `desktop.json`
//! ([`crate::state::PanelState`]); the views behind its tabs live here, are
//! built when a tab is first shown, and are dropped with their owner.

mod changes;
mod file_view;
mod files;
mod git;
mod layout;
mod lifecycle;
mod record;
mod record_sections;
mod render;
mod state;
mod viewer;

pub use changes::ChangesView;
pub use file_view::FileView;
pub use files::{FilesEvent, FilesView};
pub use git::GitSnapshot;
pub use layout::PanelPresentation;
pub use lifecycle::{PanelHost, PanelView};
pub use record::{PromptMode, RecordSection, RecordView};
pub use state::PanelOwner;
pub use viewer::{ViewerSource, ViewerState};

/// Splits a root-relative path into its directory (empty at the root) and file name.
pub(super) fn split_path(path: &str) -> (&str, &str) {
    path.rsplit_once('/').unwrap_or(("", path))
}
