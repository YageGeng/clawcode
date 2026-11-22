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
/// Largest patch of one file a diff returns, in bytes.
pub const WORKSPACE_DIFF_LIMIT: usize = 1024 * 1024;
/// Bytes at the start of a file or patch inspected to tell binary content.
const BINARY_PROBE_BYTES: usize = 8 * 1024;
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

/// Parameters of one file's diff.
#[derive(
    Debug,
    Clone,
    PartialEq,
    Eq,
    Serialize,
    Deserialize,
    typed_builder::TypedBuilder,
)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkspaceDiffParameters {
    /// Absolute workspace directory.
    pub root: PathBuf,
    /// Root-relative path of the file.
    pub path: String,
    /// The file's path before a rename, so the diff shows the rename.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[builder(default)]
    pub original_path: Option<String>,
    /// The path's bytes in hex when its name is not UTF-8.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[builder(default)]
    pub path_bytes: Option<String>,
    /// Which changes to show.
    #[serde(default)]
    #[builder(default)]
    pub scope: WorkspaceDiffScope,
}

impl WorkspaceDiffParameters {
    /// The file's root-relative path as the file system knows it.
    pub fn file_path(&self) -> PathBuf {
        file_path(&self.path, self.path_bytes.as_deref())
    }
}

/// Parameters of a change list: every changed file of one scope.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkspaceChangesParameters {
    /// Absolute workspace directory.
    pub root: PathBuf,
    /// Which changes to list.
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
    /// The path's bytes in hex when they are not UTF-8, which `path` can
    /// only show lossily; reading the file goes by these.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[builder(default)]
    pub path_bytes: Option<String>,
}

impl WorkspaceChange {
    /// The file's root-relative path as the file system knows it: its raw
    /// bytes when the name is not UTF-8.
    pub fn file_path(&self) -> PathBuf {
        file_path(&self.path, self.path_bytes.as_deref())
    }
}

/// A root-relative path from its shown form and, for a name that is not
/// UTF-8, its bytes in hex; malformed hex falls back to the shown form.
pub fn file_path(path: &str, bytes: Option<&str>) -> PathBuf {
    #[cfg(unix)]
    if let Some(bytes) = bytes.and_then(decode_hex) {
        use std::os::unix::ffi::OsStringExt as _;
        return PathBuf::from(std::ffi::OsString::from_vec(bytes));
    }
    #[cfg(not(unix))]
    let _ = bytes;
    PathBuf::from(path)
}

/// A path git wrote as raw bytes: its lossy text and, when the bytes are
/// not UTF-8, the bytes in hex, so the file can still be opened.
pub fn git_path(bytes: &[u8]) -> (String, Option<String>) {
    match std::str::from_utf8(bytes) {
        Ok(text) => (text.to_owned(), None),
        Err(_) => (
            String::from_utf8_lossy(bytes).into_owned(),
            Some(bytes.iter().map(|byte| format!("{byte:02x}")).collect()),
        ),
    }
}

/// Bytes from their hex form; `None` when it is not hex.
fn decode_hex(hex: &str) -> Option<Vec<u8>> {
    if !hex.len().is_multiple_of(2) {
        return None;
    }
    (0..hex.len())
        .step_by(2)
        .map(|at| u8::from_str_radix(hex.get(at..at + 2)?, 16).ok())
        .collect()
}

/// Every changed file of one scope, with line counts, as a diff lists them.
#[derive(
    Debug,
    Clone,
    Default,
    PartialEq,
    Eq,
    Serialize,
    Deserialize,
    typed_builder::TypedBuilder,
)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceChanges {
    /// Changed files in diff order, at most [`WORKSPACE_CHANGE_LIMIT`].
    #[builder(default)]
    pub changes: Vec<WorkspaceChange>,
    /// Whether files past the limit were left out.
    #[builder(default)]
    pub truncated: bool,
    /// Lines added across every counted file.
    #[builder(default)]
    pub insertions: u64,
    /// Lines removed across every counted file.
    #[builder(default)]
    pub deletions: u64,
}

impl WorkspaceChanges {
    /// Parses `git diff --raw --numstat -z -M` output, keeping at most `limit` files.
    ///
    /// Raw records (`:mode mode sha sha STATUS` then one path, or two for a
    /// rename or copy) give each file and its status; the numstat records
    /// after them (`added\tremoved\tpath`, or an empty path followed by the
    /// old and new paths for a rename) give its line counts, `-` for binary.
    pub fn from_raw_numstat(output: &[u8], limit: usize) -> Self {
        let mut records = output
            .split(|byte| *byte == 0)
            .filter(|record| !record.is_empty())
            .peekable();
        let mut changes = Self::default();
        // Each kept file's position by its path after the change.
        let mut index: std::collections::HashMap<String, usize> =
            std::collections::HashMap::new();
        while let Some(record) =
            records.next_if(|record| record.first() == Some(&b':'))
        {
            let record = String::from_utf8_lossy(record);
            let status = record.split(' ').nth(4).unwrap_or_default();
            let kind = match status.chars().next() {
                Some('A' | 'C') => WorkspaceChangeKind::Added,
                Some('D') => WorkspaceChangeKind::Deleted,
                Some('R') => WorkspaceChangeKind::Renamed,
                Some('U') => WorkspaceChangeKind::Conflicted,
                _ => WorkspaceChangeKind::Modified,
            };
            let first = records.next();
            // Renames and copies name the old path first, then the new one.
            let (original, path) = if status.starts_with(['R', 'C']) {
                (first, records.next())
            } else {
                (None, first)
            };
            let Some(path) = path else {
                break;
            };
            let (path, path_bytes) = git_path(path);
            // An unmerged file comes once as `U` and again as its work-tree
            // change: one file, and still a conflict.
            if let Some(change) =
                index.get(&path).and_then(|at| changes.changes.get_mut(*at))
            {
                if kind == WorkspaceChangeKind::Conflicted {
                    change.kind = kind;
                }
                continue;
            }
            if changes.changes.len() >= limit {
                changes.truncated = true;
                continue;
            }
            index.insert(path.clone(), changes.changes.len());
            changes.changes.push(
                WorkspaceChange::builder()
                    .path(path)
                    .original_path(
                        original
                            .filter(|_| kind == WorkspaceChangeKind::Renamed)
                            .map(|original| git_path(original).0),
                    )
                    .kind(kind)
                    .path_bytes(path_bytes)
                    .build(),
            );
        }
        while let Some(record) = records.next() {
            let record = String::from_utf8_lossy(record);
            let mut fields = record.splitn(3, '\t');
            let (Some(added), Some(removed), Some(path)) =
                (fields.next(), fields.next(), fields.next())
            else {
                continue;
            };
            // An empty path is a rename: the old and new paths follow.
            let path = if path.is_empty() {
                let _old = records.next();
                match records.next() {
                    Some(new) => git_path(new).0,
                    None => break,
                }
            } else {
                path.to_owned()
            };
            let Some(change) =
                index.get(&path).and_then(|at| changes.changes.get_mut(*at))
            else {
                continue;
            };
            // `-` marks a binary file, which has no line counts; a file with
            // two records sums their counts.
            let (Ok(added), Ok(removed)) =
                (added.parse::<u64>(), removed.parse::<u64>())
            else {
                change.insertions = None;
                change.deletions = None;
                continue;
            };
            change.insertions = Some(change.insertions.unwrap_or(0) + added);
            change.deletions = Some(change.deletions.unwrap_or(0) + removed);
            changes.insertions += added;
            changes.deletions += removed;
        }
        changes
    }
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

impl WorkspaceDiff {
    /// Turns one file's raw `git diff` output into a patch, a binary notice,
    /// or no change; `over_limit` says its producer was stopped at the limit.
    pub fn from_git_output(output: Vec<u8>, over_limit: bool) -> Self {
        if output.is_empty() {
            return Self::Unchanged;
        }
        // A binary diff is a header and one "Binary files … differ" line, so
        // the head of the output decides it without scanning the whole patch.
        let head = output.get(..BINARY_PROBE_BYTES).unwrap_or(&output);
        let head = String::from_utf8_lossy(head);
        if head.contains("Binary files") && !head.contains("@@") {
            return Self::Binary;
        }
        Self::patch(output, over_limit)
    }

    /// A patch cut at the last whole line within [`WORKSPACE_DIFF_LIMIT`] when
    /// it is longer or its producer was stopped there.
    fn patch(mut output: Vec<u8>, over_limit: bool) -> Self {
        let truncated = over_limit || output.len() > WORKSPACE_DIFF_LIMIT;
        if truncated {
            let limit = output.get(..WORKSPACE_DIFF_LIMIT).unwrap_or(&output);
            let end = limit
                .iter()
                .rposition(|byte| *byte == b'\n')
                .map_or(limit.len(), |newline| newline + 1);
            output.truncate(end);
        }
        // Valid UTF-8 moves into the String without a copy.
        let patch = String::from_utf8(output).unwrap_or_else(|error| {
            String::from_utf8_lossy(error.as_bytes()).into_owned()
        });
        Self::Patch { patch, truncated }
    }
}
