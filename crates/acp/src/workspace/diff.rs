//! Patches of the workspace against its diff base or the index.

use std::ffi::OsStr;

use protocol::{
    WORKSPACE_CHANGE_LIMIT, WORKSPACE_DIFF_LIMIT, WorkspaceDiff,
    WorkspaceDiffScope,
};

use super::git::LimitedOutput;
use super::{BINARY_PROBE_BYTES, WorkspaceError, WorkspaceRoot};

/// Git's empty tree: the diff base of a repository without commits.
const EMPTY_TREE: &str = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";

/// What line counts and the default diff compare the work tree against.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct DiffBase {
    /// Revision handed to `git diff`.
    pub(super) rev: String,
    /// Name shown to the user, e.g. `origin/main` or `HEAD`.
    pub(super) label: String,
}

impl WorkspaceRoot {
    /// The diff base: the upstream's merge base, else `HEAD`, else the empty tree.
    pub(super) async fn diff_base(&self) -> Result<DiffBase, WorkspaceError> {
        let head = self
            .git(&["rev-parse", "--verify", "--quiet", "HEAD"])
            .await?;
        if !head.status.success() {
            return Ok(DiffBase {
                rev: EMPTY_TREE.to_owned(),
                label: "HEAD".to_owned(),
            });
        }
        let upstream = self
            .git(&[
                "rev-parse",
                "--abbrev-ref",
                "--symbolic-full-name",
                "@{upstream}",
            ])
            .await?;
        if upstream.status.success() {
            let merge_base =
                self.git(&["merge-base", "HEAD", "@{upstream}"]).await?;
            if merge_base.status.success() {
                return Ok(DiffBase {
                    rev: Self::trimmed(&merge_base.stdout),
                    label: Self::trimmed(&upstream.stdout),
                });
            }
        }
        Ok(DiffBase {
            rev: "HEAD".to_owned(),
            label: "HEAD".to_owned(),
        })
    }

    /// A diff of one file, or of every changed file when `relative` is `None`.
    ///
    /// `Base` compares the work tree with the diff base, `Unstaged` with the
    /// index. Untracked files are shown as added in both scopes; deleted files
    /// are included. The patch stops at [`WORKSPACE_DIFF_LIMIT`].
    pub async fn git_diff(
        &self,
        relative: Option<&str>,
        scope: WorkspaceDiffScope,
    ) -> Result<WorkspaceDiff, WorkspaceError> {
        let rev = match scope {
            WorkspaceDiffScope::Base => Some(self.diff_base().await?.rev),
            WorkspaceDiffScope::Unstaged => None,
        };
        match relative {
            Some(relative) => self.file_diff(relative, rev.as_deref()).await,
            None => self.workspace_diff(rev.as_deref()).await,
        }
    }

    /// One file's diff; an untracked file is diffed against `/dev/null`.
    pub(super) async fn file_diff(
        &self,
        relative: &str,
        rev: Option<&str>,
    ) -> Result<WorkspaceDiff, WorkspaceError> {
        let path = Self::checked(relative)?;
        // A deleted file no longer resolves; its path was checked lexically above.
        let exists = match self.resolve(relative) {
            Ok(_) => true,
            Err(WorkspaceError::NotFound) => false,
            Err(error) => return Err(error),
        };
        let path = path.as_os_str();
        let tracked = self
            .git(&[
                OsStr::new("ls-files"),
                OsStr::new("--error-unmatch"),
                OsStr::new("--"),
                path,
            ])
            .await?
            .status
            .success();
        let output = if tracked {
            let mut args = Self::diff_args(rev);
            args.extend([OsStr::new("--"), path]);
            self.git_limited(&args).await?
        } else if exists {
            self.untracked_diff(path).await?
        } else {
            return Err(WorkspaceError::NotFound);
        };
        output.check()?;
        Ok(Self::classify(output.stdout, output.over_limit))
    }

    /// Every changed file's diff, tracked files first, then untracked ones,
    /// stopping once the patch passes the size limit.
    pub(super) async fn workspace_diff(
        &self,
        rev: Option<&str>,
    ) -> Result<WorkspaceDiff, WorkspaceError> {
        let tracked = self.git_limited(&Self::diff_args(rev)).await?;
        tracked.check()?;
        let mut patch = tracked.stdout;
        let mut over_limit = tracked.over_limit;
        if !over_limit {
            let untracked = self
                .git(&["ls-files", "--others", "--exclude-standard", "-z"])
                .await?;
            if !untracked.status.success() {
                return Err(Self::failure(&untracked.stderr));
            }
            // Untracked files are written as patches here rather than one
            // `git diff --no-index` each: thousands of them stay one pass.
            let files: Vec<Vec<u8>> = untracked
                .stdout
                .split(|byte| *byte == 0)
                .filter(|file| !file.is_empty())
                .take(WORKSPACE_CHANGE_LIMIT)
                .map(<[u8]>::to_vec)
                .collect();
            let root = self.clone();
            let budget = WORKSPACE_DIFF_LIMIT.saturating_sub(patch.len());
            let (untracked, over) = tokio::task::spawn_blocking(move || {
                root.untracked_patch(&files, budget)
            })
            .await
            .map_err(|error| WorkspaceError::Io(error.to_string()))?;
            patch.extend_from_slice(&untracked);
            over_limit = over;
        }
        // Several files may mix text and binary, so only emptiness is special here.
        if patch.is_empty() {
            return Ok(WorkspaceDiff::Unchanged);
        }
        Ok(Self::patch(patch, over_limit))
    }

    /// Added-file patches of untracked files (root-relative, as `ls-files`
    /// names them), in at most `budget` bytes; `true` when they did not fit.
    /// A file that cannot be read any more is left out.
    pub(super) fn untracked_patch(
        &self,
        files: &[Vec<u8>],
        budget: usize,
    ) -> (Vec<u8>, bool) {
        use std::io::{Read as _, Write as _};
        use std::os::unix::ffi::OsStrExt as _;

        let mut patch = Vec::new();
        for file in files {
            let name = String::from_utf8_lossy(file);
            let path = self.0.join(OsStr::from_bytes(file));
            // Read no more than still fits, plus one byte to know it did not.
            let room = budget.saturating_sub(patch.len());
            let Ok(handle) = std::fs::File::open(&path) else {
                continue;
            };
            let mut bytes = Vec::new();
            if handle
                .take(room as u64 + 1)
                .read_to_end(&mut bytes)
                .is_err()
            {
                continue;
            }
            let _ = write!(
                patch,
                "diff --git a/{name} b/{name}\nnew file mode 100644\n"
            );
            let probe = bytes.get(..BINARY_PROBE_BYTES).unwrap_or(&bytes);
            if probe.contains(&0) {
                let _ = writeln!(
                    patch,
                    "Binary files /dev/null and b/{name} differ"
                );
            } else if !bytes.is_empty() {
                let text = String::from_utf8_lossy(&bytes);
                let lines: Vec<&str> = text.lines().collect();
                let _ = write!(
                    patch,
                    "--- /dev/null\n+++ b/{name}\n@@ -0,0 +1,{} @@\n",
                    lines.len()
                );
                for line in &lines {
                    let _ = writeln!(patch, "+{line}");
                }
                if !text.ends_with('\n') {
                    patch.extend_from_slice(b"\\ No newline at end of file\n");
                }
            }
            if patch.len() > budget {
                return (patch, true);
            }
        }
        (patch, false)
    }

    /// The patch of an untracked file: everything added, against `/dev/null`.
    pub(super) async fn untracked_diff(
        &self,
        path: &OsStr,
    ) -> Result<LimitedOutput, WorkspaceError> {
        self.git_limited(&[
            OsStr::new("diff"),
            OsStr::new("--no-color"),
            OsStr::new("--no-ext-diff"),
            OsStr::new("--no-index"),
            OsStr::new("--"),
            OsStr::new("/dev/null"),
            path,
        ])
        .await
    }

    /// `git diff` arguments for tracked files, against `rev` or the index.
    pub(super) fn diff_args(rev: Option<&str>) -> Vec<&OsStr> {
        let mut args = vec![
            OsStr::new("diff"),
            OsStr::new("--no-color"),
            OsStr::new("--no-ext-diff"),
            OsStr::new("--relative"),
        ];
        args.extend(rev.map(OsStr::new));
        args
    }

    /// Turns one file's raw `git diff` output into a patch, a binary notice, or no change.
    pub(super) fn classify(output: Vec<u8>, over_limit: bool) -> WorkspaceDiff {
        if output.is_empty() {
            return WorkspaceDiff::Unchanged;
        }
        // A binary diff is a header and one "Binary files … differ" line, so
        // the head of the output decides it without scanning the whole patch.
        let head = output.get(..BINARY_PROBE_BYTES).unwrap_or(&output);
        let head = String::from_utf8_lossy(head);
        if head.contains("Binary files") && !head.contains("@@") {
            return WorkspaceDiff::Binary;
        }
        Self::patch(output, over_limit)
    }

    /// A patch cut at the last whole line within [`WORKSPACE_DIFF_LIMIT`] when
    /// it is longer or its producer was stopped there.
    pub(super) fn patch(
        mut output: Vec<u8>,
        over_limit: bool,
    ) -> WorkspaceDiff {
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
        WorkspaceDiff::Patch { patch, truncated }
    }
}
