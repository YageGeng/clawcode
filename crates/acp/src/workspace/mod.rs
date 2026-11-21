//! Read-only workspace tools behind `_clawcode/workspace/*`: directory
//! listings, file previews, Git status, and per-file diffs.
//!
//! Every operation hangs off a [`WorkspaceRoot`], a validated and
//! canonicalized directory; client paths are resolved against it and may
//! never leave it, symbolic links included.

mod diff;
mod git;
mod status;

use std::path::{Component, Path, PathBuf};

use protocol::{
    AcpWorkingDirectory, AcpWorkingDirectoryError, WORKSPACE_ENTRY_LIMIT,
    WORKSPACE_FILE_LIMIT, WorkspaceDirEntry, WorkspaceDirListing,
    WorkspaceEntryKind, WorkspaceFileContent,
};

/// Bytes inspected for a NUL when deciding whether a file is binary.
const BINARY_PROBE_BYTES: usize = 8 * 1024;

/// Why a workspace request could not be answered.
#[derive(Debug, thiserror::Error)]
pub enum WorkspaceError {
    /// The root is not an absolute existing directory.
    #[error("invalid workspace root: {0}")]
    Root(AcpWorkingDirectoryError),
    /// The path is absolute, climbs with `..`, or resolves outside the root.
    #[error("path is outside the workspace")]
    OutsideRoot,
    /// The path does not exist.
    #[error("path does not exist")]
    NotFound,
    /// Reading the file system failed.
    #[error("{0}")]
    Io(String),
    /// Running `git` failed, timed out, or `git` is not installed.
    #[error("git failed: {0}")]
    Git(String),
}

impl From<WorkspaceError> for agent_client_protocol::Error {
    /// Client mistakes are invalid parameters; file-system and Git failures are internal.
    fn from(error: WorkspaceError) -> Self {
        let message = error.to_string();
        match error {
            WorkspaceError::Root(_)
            | WorkspaceError::OutsideRoot
            | WorkspaceError::NotFound => Self::invalid_params().data(message),
            WorkspaceError::Io(_) | WorkspaceError::Git(_) => {
                Self::internal_error().data(message)
            }
        }
    }
}

/// A validated, canonical workspace directory that owns every read operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceRoot(PathBuf);

impl TryFrom<&Path> for WorkspaceRoot {
    type Error = WorkspaceError;

    /// Accepts an absolute existing directory and canonicalizes it.
    fn try_from(path: &Path) -> Result<Self, WorkspaceError> {
        let directory = AcpWorkingDirectory::try_from(path.to_path_buf())
            .map_err(WorkspaceError::Root)?;
        directory
            .into_inner()
            .canonicalize()
            .map(Self)
            .map_err(|error| WorkspaceError::Io(error.to_string()))
    }
}

impl WorkspaceRoot {
    /// Resolves a root-relative path to an existing canonical path inside the root.
    ///
    /// The path is checked lexically first (no absolute path, no `..`), then
    /// canonicalized, so a symbolic link pointing outside is rejected too.
    pub fn resolve(&self, relative: &str) -> Result<PathBuf, WorkspaceError> {
        let joined = self.0.join(Self::checked(relative)?);
        let resolved =
            joined.canonicalize().map_err(|error| match error.kind() {
                std::io::ErrorKind::NotFound => WorkspaceError::NotFound,
                _ => WorkspaceError::Io(error.to_string()),
            })?;
        if resolved.starts_with(&self.0) {
            Ok(resolved)
        } else {
            Err(WorkspaceError::OutsideRoot)
        }
    }

    /// The lexical check of a client path: relative, without `..` or a root.
    fn checked(relative: &str) -> Result<&Path, WorkspaceError> {
        let path = Path::new(relative);
        let escapes = path.components().any(|component| {
            !matches!(component, Component::Normal(_) | Component::CurDir)
        });
        if escapes {
            Err(WorkspaceError::OutsideRoot)
        } else {
            Ok(path)
        }
    }

    /// Lists one directory: directories first, then files, each by name.
    ///
    /// `.gitignore` rules apply, `.git` is never listed, and names starting
    /// with `.` are listed only when `show_hidden` is set.
    pub fn list_dir(
        &self,
        relative: &str,
        show_hidden: bool,
    ) -> Result<WorkspaceDirListing, WorkspaceError> {
        let directory = self.resolve(relative)?;
        let walker = ignore::WalkBuilder::new(&directory)
            .max_depth(Some(1))
            .hidden(!show_hidden)
            .git_ignore(true)
            .git_exclude(true)
            .parents(true)
            .follow_links(false)
            .build();
        let mut entries = Vec::new();
        for entry in walker {
            let entry =
                entry.map_err(|error| WorkspaceError::Io(error.to_string()))?;
            if entry.depth() == 0 || entry.file_name() == ".git" {
                continue;
            }
            let kind = match entry.file_type() {
                Some(kind) if kind.is_symlink() => WorkspaceEntryKind::Symlink,
                Some(kind) if kind.is_dir() => WorkspaceEntryKind::Directory,
                _ => WorkspaceEntryKind::File,
            };
            entries.push(WorkspaceDirEntry {
                name: entry.file_name().to_string_lossy().into_owned(),
                path: self.relative(entry.path()),
                kind,
            });
        }
        entries.sort_by_cached_key(|entry| {
            (
                entry.kind != WorkspaceEntryKind::Directory,
                entry.name.to_lowercase(),
            )
        });
        let truncated = entries.len() > WORKSPACE_ENTRY_LIMIT;
        entries.truncate(WORKSPACE_ENTRY_LIMIT);
        Ok(WorkspaceDirListing { entries, truncated })
    }

    /// Reads a file for preview; large and binary files are reported, not returned.
    pub fn read_file(
        &self,
        relative: &str,
    ) -> Result<WorkspaceFileContent, WorkspaceError> {
        let path = self.resolve(relative)?;
        let size = std::fs::metadata(&path)
            .map_err(|error| WorkspaceError::Io(error.to_string()))?
            .len();
        // The size is checked from the metadata, so a huge file is never read.
        if size > WORKSPACE_FILE_LIMIT {
            return Ok(WorkspaceFileContent::TooLarge { size });
        }
        let bytes = std::fs::read(&path)
            .map_err(|error| WorkspaceError::Io(error.to_string()))?;
        let probe = bytes.get(..BINARY_PROBE_BYTES).unwrap_or(&bytes);
        if probe.contains(&0) {
            return Ok(WorkspaceFileContent::Binary);
        }
        Ok(String::from_utf8(bytes)
            .map_or(WorkspaceFileContent::Binary, |text| {
                WorkspaceFileContent::Text { text }
            }))
    }

    /// The root-relative, `/`-separated form of a path inside the root.
    fn relative(&self, path: &Path) -> String {
        path.strip_prefix(&self.0)
            .unwrap_or(path)
            .components()
            .map(|component| component.as_os_str().to_string_lossy())
            .collect::<Vec<_>>()
            .join("/")
    }
}
