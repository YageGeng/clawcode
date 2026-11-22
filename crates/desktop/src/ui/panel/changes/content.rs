//! What one scope of the Changes tab holds: its changed files, the patches
//! that arrived, and the tables its file headers and tree draw from, kept up
//! to date as reads land rather than rebuilt on every frame.

use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use gpui_kit::Entity;
use gpui_kit::Hsla;
use gpui_kit::component::Theme;
use gpui_kit::component::diff::{DiffFile, DiffParseOptions, DiffState};
use protocol::{
    WorkspaceChange, WorkspaceChangeKind, WorkspaceChanges, WorkspaceDiff,
};
use typed_builder::TypedBuilder;

use super::super::ViewerState;
use crate::backend::BackendError;
use crate::file::PatchLoader;

/// Where one file's patch stands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileNote {
    /// Not requested yet, or on the way.
    Loading,
    /// Shown in full.
    Loaded,
    /// Shown up to the size limit.
    TooLarge,
    /// Binary content: no lines to show.
    Binary,
    /// The read failed; carries the error.
    Failed(String),
}

/// What each file header shows, by path: the change and its patch's state.
pub type HeaderInfo = Rc<HashMap<String, (WorkspaceChange, FileNote)>>;

/// The tree's files by path: their place in the list and their change.
pub type TreeFiles = Rc<HashMap<String, (usize, WorkspaceChange)>>;

/// The theme color of a change's status; the tree colors the file's name
/// and status letter with it (M3 plan Global Constraints).
pub fn kind_color(kind: WorkspaceChangeKind, theme: &Theme) -> Hsla {
    match kind {
        WorkspaceChangeKind::Added | WorkspaceChangeKind::Untracked => {
            theme.success
        }
        WorkspaceChangeKind::Deleted | WorkspaceChangeKind::Conflicted => {
            theme.danger
        }
        WorkspaceChangeKind::Modified => theme.warning,
        WorkspaceChangeKind::Renamed => theme.info,
    }
}

/// What one scope shows: its files, their patches so far, and the diff.
#[derive(TypedBuilder)]
pub struct ScopeContent {
    /// The changed files and which patches arrived or are on the way.
    #[builder(default)]
    pub(super) loader: PatchLoader,
    /// Whether files past the list limit were left out.
    #[builder(default)]
    pub(super) truncated: bool,
    /// Lines added and removed across every counted file.
    #[builder(default)]
    pub(super) totals: (u64, u64),
    /// One header-only placeholder per file, in list order.
    #[builder(default)]
    placeholders: Vec<Option<DiffFile>>,
    /// Arrived patches by path.
    #[builder(default)]
    patches: HashMap<String, DiffFile>,
    /// Each file's patch state by path.
    #[builder(default)]
    pub(super) notes: HashMap<String, FileNote>,
    /// What the file headers show; rebuilt when files or notes change.
    #[builder(default)]
    pub(super) headers: HeaderInfo,
    /// What the tree shows; replaced only by a different file list, so an
    /// unchanged list keeps the tree and its selection.
    #[builder(default)]
    pub(super) tree_files: TreeFiles,
    /// The diff of every file, placeholders where no patch arrived.
    #[builder(default)]
    pub(super) diff: Option<Entity<DiffState>>,
    /// Where the content stands; `None` before the first read.
    #[builder(default)]
    pub(super) state: Option<ViewerState>,
}

impl Default for ScopeContent {
    /// Nothing read yet.
    fn default() -> Self {
        Self::builder().build()
    }
}

impl ScopeContent {
    /// The files to show: each arrived patch, else its placeholder.
    pub(super) fn diff_files(&self) -> Vec<DiffFile> {
        self.loader
            .changes()
            .iter()
            .zip(&self.placeholders)
            .filter_map(|(change, placeholder)| {
                self.patches
                    .get(&change.path)
                    .cloned()
                    .or_else(|| placeholder.clone())
            })
            .collect()
    }

    /// Records one file's read: its parsed patch, or why there is none. The
    /// header table is rebuilt once the batch is in, by
    /// [`Self::refresh_headers`].
    pub(super) fn take_patch(
        &mut self,
        path: &str,
        result: Result<WorkspaceDiff, BackendError>,
    ) {
        self.loader.finished(path);
        let note = match result {
            Ok(WorkspaceDiff::Patch { patch, truncated }) => {
                // A patch cut at the size limit ends mid-hunk; let the parser accept that.
                let options =
                    DiffParseOptions::new().truncated_hunks(truncated);
                match DiffFile::parse_with(&patch, options)
                    .map(|files| files.into_iter().next())
                {
                    Ok(Some(file)) => {
                        self.patches.insert(path.to_owned(), file);
                        if truncated {
                            FileNote::TooLarge
                        } else {
                            FileNote::Loaded
                        }
                    }
                    Ok(None) => FileNote::Loaded,
                    Err(error) => {
                        tracing::warn!(
                            "unreadable diff of {}: {}",
                            path,
                            error
                        );
                        FileNote::Failed(error.to_string())
                    }
                }
            }
            Ok(WorkspaceDiff::Binary) => FileNote::Binary,
            Ok(WorkspaceDiff::Unchanged) => FileNote::Loaded,
            Err(error) => {
                tracing::warn!("failed to diff {}: {}", path, error);
                FileNote::Failed(error.to_string())
            }
        };
        self.notes.insert(path.to_owned(), note);
    }

    /// Takes a new file list, keeping each arrived patch whose change is the
    /// same; the others, and any whose read failed, load again. Returns
    /// whether the diff's files differ from before, so an unchanged list
    /// leaves the diff, and its scroll position, alone.
    pub(super) fn take_files(&mut self, changes: &WorkspaceChanges) -> bool {
        // A failed read is tried again on every new list.
        let failed = self.notes.len();
        self.notes
            .retain(|_, note| !matches!(note, FileNote::Failed(_)));
        let retried = self.notes.len() != failed;
        let same_list = self.loader.changes() == changes.changes.as_slice();
        let kept: HashSet<&str> = changes
            .changes
            .iter()
            .filter(|change| self.loader.change(&change.path) == Some(*change))
            .map(|change| change.path.as_str())
            .collect();
        self.patches.retain(|path, _| kept.contains(path.as_str()));
        self.notes.retain(|path, _| kept.contains(path.as_str()));
        if !same_list {
            self.tree_files = Rc::new(
                changes
                    .changes
                    .iter()
                    .enumerate()
                    .map(|(index, change)| {
                        (change.path.clone(), (index, change.clone()))
                    })
                    .collect(),
            );
        }
        self.loader = PatchLoader::new(changes);
        for path in self.notes.keys() {
            self.loader.finished(path);
        }
        self.placeholders = self
            .loader
            .placeholder_patches()
            .iter()
            .zip(self.loader.changes())
            .map(|(patch, change)| Self::placeholder(patch, &change.path))
            .collect();
        self.truncated = changes.truncated;
        self.totals = (changes.insertions, changes.deletions);
        self.refresh_headers();
        !same_list || retried || self.diff.is_none()
    }

    /// Drops every patch so each loads again: an edit that keeps a file's
    /// line counts would otherwise keep showing the old patch.
    pub(super) fn forget_patches(&mut self) {
        self.patches.clear();
        self.notes.clear();
        self.loader.forget_all();
    }

    /// One file's placeholder from its header-only `patch`. A name that
    /// does not survive in a patch header, such as one holding a newline,
    /// gets a header with those characters replaced, so the file still has
    /// its row in the diff; `None` only if even that does not parse.
    fn placeholder(patch: &str, path: &str) -> Option<DiffFile> {
        let first = |patch: &str| {
            DiffFile::parse(patch)
                .ok()
                .and_then(|files| files.into_iter().next())
        };
        first(patch).or_else(|| {
            let shown: String = path
                .chars()
                .map(|c| if c.is_control() || c == '"' { '?' } else { c })
                .collect();
            first(&format!("diff --git a/{shown} b/{shown}\n"))
        })
    }

    /// Rebuilds what the file headers show from the files and their notes.
    pub(super) fn refresh_headers(&mut self) {
        self.headers = Rc::new(
            self.loader
                .changes()
                .iter()
                .map(|change| {
                    let note = self
                        .notes
                        .get(&change.path)
                        .cloned()
                        .unwrap_or(FileNote::Loading);
                    (change.path.clone(), (change.clone(), note))
                })
                .collect(),
        );
    }
}
