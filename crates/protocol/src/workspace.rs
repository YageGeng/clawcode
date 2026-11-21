//! Read-only workspace tools a client reaches through `_clawcode/workspace/*`:
//! directory listings, file previews, Git status, and per-file diffs.
//!
//! Paths in requests and answers are relative to the workspace root and use
//! `/` separators; the empty string is the root itself.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Most entries one directory listing returns.
pub const WORKSPACE_ENTRY_LIMIT: usize = 2000;
/// Largest file a preview returns, in bytes.
pub const WORKSPACE_FILE_LIMIT: u64 = 1024 * 1024;
/// Largest patch a diff returns, in bytes.
pub const WORKSPACE_DIFF_LIMIT: usize = 2 * 1024 * 1024;
/// Most changes one Git status returns.
pub const WORKSPACE_CHANGE_LIMIT: usize = 5000;

/// Parameters naming only a workspace root.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkspaceRootParameters {
    /// Absolute workspace directory.
    pub root: PathBuf,
}

/// Parameters naming one path inside a workspace.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkspacePathParameters {
    /// Absolute workspace directory.
    pub root: PathBuf,
    /// Root-relative, `/`-separated path.
    pub path: String,
}

/// Parameters of a directory listing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkspaceListDirParameters {
    /// Absolute workspace directory.
    pub root: PathBuf,
    /// Root-relative directory; empty for the root.
    pub path: String,
    /// Whether names starting with `.` are listed.
    #[serde(default)]
    pub show_hidden: bool,
}

/// Which changes a diff shows.
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceDiffScope {
    /// The work tree against the diff base (the upstream's merge base, else `HEAD`).
    #[default]
    Base,
    /// The work tree against the index: changes not staged yet.
    Unstaged,
}

/// Parameters of a diff: one file, or the whole workspace when `path` is absent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkspaceDiffParameters {
    /// Absolute workspace directory.
    pub root: PathBuf,
    /// Root-relative path of one file; `None` diffs every changed file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// Which changes to show.
    #[serde(default)]
    pub scope: WorkspaceDiffScope,
}

/// What a listed entry is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceEntryKind {
    /// A directory.
    Directory,
    /// A regular file.
    File,
    /// A symbolic link, not followed.
    Symlink,
}

/// One entry of a directory listing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceDirEntry {
    /// File name.
    pub name: String,
    /// Root-relative path.
    pub path: String,
    /// What the entry is.
    pub kind: WorkspaceEntryKind,
}

/// One directory's entries: directories first, then files, each by name.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceDirListing {
    /// The entries, at most [`WORKSPACE_ENTRY_LIMIT`].
    pub entries: Vec<WorkspaceDirEntry>,
    /// Whether entries past the limit were left out.
    pub truncated: bool,
}

/// A file's previewable content.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WorkspaceFileContent {
    /// UTF-8 text.
    Text {
        /// The whole file.
        text: String,
    },
    /// Not text: a NUL byte near the start, or not UTF-8.
    Binary,
    /// Larger than [`WORKSPACE_FILE_LIMIT`]; nothing was read.
    TooLarge {
        /// File size in bytes.
        size: u64,
    },
}

/// How a file differs from the last commit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceChangeKind {
    /// Changed content or type.
    Modified,
    /// New in the index, or copied.
    Added,
    /// Removed.
    Deleted,
    /// Renamed, possibly with changes.
    Renamed,
    /// Not tracked by Git.
    Untracked,
    /// Unmerged after a conflict.
    Conflicted,
}

impl WorkspaceChangeKind {
    /// The one-letter status shown in change lists.
    pub fn letter(self) -> char {
        match self {
            Self::Modified => 'M',
            Self::Added => 'A',
            Self::Deleted => 'D',
            Self::Renamed => 'R',
            Self::Untracked => '?',
            Self::Conflicted => 'U',
        }
    }
}

/// One changed file.
#[derive(
    Debug,
    Clone,
    PartialEq,
    Eq,
    Serialize,
    Deserialize,
    typed_builder::TypedBuilder,
)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceChange {
    /// Root-relative path, after a rename.
    pub path: String,
    /// Path before a rename.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[builder(default)]
    pub original_path: Option<String>,
    /// How it changed.
    pub kind: WorkspaceChangeKind,
    /// Lines added against the diff base; `None` for binary files or when not counted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[builder(default)]
    pub insertions: Option<u64>,
    /// Lines removed against the diff base; `None` for binary files or when not counted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[builder(default)]
    pub deletions: Option<u64>,
}

/// A workspace's Git state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WorkspaceGitStatus {
    /// A Git work tree.
    Repository {
        /// Checked-out branch; `None` when detached.
        branch: Option<String>,
        /// What line counts and the default diff compare against, e.g.
        /// `origin/main` (the upstream's merge base) or `HEAD`.
        #[serde(default)]
        base: String,
        /// Changed files, at most [`WORKSPACE_CHANGE_LIMIT`].
        changes: Vec<WorkspaceChange>,
        /// Whether changes past the limit were left out.
        truncated: bool,
        /// Lines added across every counted file.
        #[serde(default)]
        insertions: u64,
        /// Lines removed across every counted file.
        #[serde(default)]
        deletions: u64,
    },
    /// The directory is not inside a Git work tree.
    NotRepository,
    /// `git` could not be run or did not answer in time.
    GitUnavailable,
}

impl WorkspaceGitStatus {
    /// Parses `git status --porcelain=v2 -z --branch --untracked-files=all`.
    ///
    /// Records are NUL-separated. Ordinary (`1`), renamed (`2`), and
    /// unmerged (`u`) records put the path after a fixed number of
    /// space-separated fields, so a path may itself contain spaces; a rename
    /// record is followed by one more record holding the original path.
    pub fn from_porcelain_v2(output: &[u8], limit: usize) -> Self {
        let mut records = output
            .split(|byte| *byte == 0)
            .filter(|record| !record.is_empty());
        let mut branch = None;
        let mut changes = Vec::new();
        let mut truncated = false;
        while let Some(record) = records.next() {
            let text = String::from_utf8_lossy(record);
            if let Some(head) = text.strip_prefix("# branch.head ") {
                branch = (head != "(detached)").then(|| head.to_owned());
                continue;
            }
            let change = match record.first() {
                Some(b'1') => Self::fields(&text, 8).map(|(xy, path)| {
                    WorkspaceChange::builder()
                        .path(path)
                        .kind(Self::kind(xy))
                        .build()
                }),
                Some(b'2') => Self::fields(&text, 9).map(|(_, path)| {
                    WorkspaceChange::builder()
                        .path(path)
                        .original_path(records.next().map(|original| {
                            String::from_utf8_lossy(original).into_owned()
                        }))
                        .kind(WorkspaceChangeKind::Renamed)
                        .build()
                }),
                Some(b'u') => Self::fields(&text, 10).map(|(_, path)| {
                    WorkspaceChange::builder()
                        .path(path)
                        .kind(WorkspaceChangeKind::Conflicted)
                        .build()
                }),
                Some(b'?') => text.strip_prefix("? ").map(|path| {
                    WorkspaceChange::builder()
                        .path(path.to_owned())
                        .kind(WorkspaceChangeKind::Untracked)
                        .build()
                }),
                _ => None,
            };
            let Some(change) = change else { continue };
            if changes.len() == limit {
                truncated = true;
                break;
            }
            changes.push(change);
        }
        Self::Repository {
            branch,
            base: String::new(),
            changes,
            truncated,
            insertions: 0,
            deletions: 0,
        }
    }

    /// Fills per-file line counts from `git diff --numstat -z` and adds up the totals.
    ///
    /// Records are `added\tremoved\tpath\0`; a rename leaves the path empty and
    /// follows with `old\0new\0`; a binary file counts `-` for both. Files
    /// the numstat does not name keep their counts.
    pub fn apply_numstat(&mut self, numstat: &[u8]) {
        let Self::Repository {
            changes,
            insertions,
            deletions,
            ..
        } = self
        else {
            return;
        };
        // Index the changes by path once: numstat may name thousands of files.
        let index: std::collections::HashMap<String, usize> = changes
            .iter()
            .enumerate()
            .map(|(position, change)| (change.path.clone(), position))
            .collect();
        let mut records = numstat.split(|byte| *byte == 0);
        while let Some(record) = records.next() {
            let text = String::from_utf8_lossy(record);
            let mut fields = text.splitn(3, '\t');
            let (Some(added), Some(removed), Some(path)) =
                (fields.next(), fields.next(), fields.next())
            else {
                continue;
            };
            // A rename names its new path in the second record after this one.
            let path = if path.is_empty() {
                let _old = records.next();
                records
                    .next()
                    .map(|new| String::from_utf8_lossy(new).into_owned())
                    .unwrap_or_default()
            } else {
                path.to_owned()
            };
            let (added, removed) = (added.parse().ok(), removed.parse().ok());
            if let Some(change) = index
                .get(&path)
                .and_then(|position| changes.get_mut(*position))
            {
                change.insertions = added;
                change.deletions = removed;
            }
            *insertions += added.unwrap_or(0);
            *deletions += removed.unwrap_or(0);
        }
    }

    /// Splits a record into its `XY` status and the path after `skip` fields.
    fn fields(record: &str, skip: usize) -> Option<(&str, String)> {
        let mut parts = record.splitn(skip + 1, ' ');
        let xy = parts.nth(1)?;
        let path = parts.nth(skip - 2)?;
        Some((xy, path.to_owned()))
    }

    /// Kind of an ordinary record: the work tree's state, else the index's.
    fn kind(xy: &str) -> WorkspaceChangeKind {
        let mut states = xy.chars();
        let staged = states.next().unwrap_or('.');
        let state = match states.next() {
            Some('.') | None => staged,
            Some(worktree) => worktree,
        };
        match state {
            'A' | 'C' => WorkspaceChangeKind::Added,
            'D' => WorkspaceChangeKind::Deleted,
            'R' => WorkspaceChangeKind::Renamed,
            _ => WorkspaceChangeKind::Modified,
        }
    }
}

/// One file's difference from the last commit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WorkspaceDiff {
    /// A unified patch.
    Patch {
        /// The patch, at most [`WORKSPACE_DIFF_LIMIT`] bytes.
        patch: String,
        /// Whether the patch was cut at the limit.
        truncated: bool,
    },
    /// Binary content: no textual patch.
    Binary,
    /// No difference.
    Unchanged,
}
