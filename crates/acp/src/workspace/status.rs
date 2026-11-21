//! The workspace's Git status: branch, changed files, and their line counts.

use std::path::{Path, PathBuf};

use protocol::{
    WORKSPACE_CHANGE_LIMIT, WORKSPACE_FILE_LIMIT, WorkspaceChangeKind,
    WorkspaceGitStatus,
};

use super::{BINARY_PROBE_BYTES, WorkspaceError, WorkspaceRoot};

/// Lines of a file, read in chunks; `None` when it looks binary, is larger
/// than [`WORKSPACE_FILE_LIMIT`], or cannot be read.
///
/// A last line without a trailing newline still counts.
fn count_file_lines(path: &Path) -> Option<u64> {
    use std::io::Read as _;

    // A file too large to preview is not read to count it either.
    if std::fs::metadata(path).ok()?.len() > WORKSPACE_FILE_LIMIT {
        return None;
    }
    let mut file = std::fs::File::open(path).ok()?;
    let mut buffer = vec![0u8; BINARY_PROBE_BYTES];
    let mut lines = 0u64;
    let mut first = true;
    let mut last = None;
    loop {
        let read = file.read(&mut buffer).ok()?;
        let chunk = buffer.get(..read)?;
        if read == 0 {
            break;
        }
        // Binary detection looks at the start only, like previews do.
        if first && chunk.contains(&0) {
            return None;
        }
        first = false;
        lines += chunk.iter().filter(|byte| **byte == b'\n').count() as u64;
        last = chunk.last().copied();
    }
    Some(lines + u64::from(last.is_some_and(|byte| byte != b'\n')))
}

impl WorkspaceRoot {
    /// The workspace's Git branch and changed files.
    ///
    /// A workspace may be a subdirectory of a larger repository: Git reports
    /// paths from the repository's top level, so the status is limited to the
    /// workspace and its prefix is stripped to keep paths root-relative.
    pub async fn git_status(&self) -> WorkspaceGitStatus {
        let probe = match self
            .git(&["rev-parse", "--is-inside-work-tree", "--show-prefix"])
            .await
        {
            Ok(output) => output,
            Err(error) => {
                tracing::warn!(
                    "git is unavailable for workspace {}: {}",
                    self.0.display(),
                    error
                );
                return WorkspaceGitStatus::GitUnavailable;
            }
        };
        if !probe.status.success() {
            return WorkspaceGitStatus::NotRepository;
        }
        let prefix = String::from_utf8_lossy(&probe.stdout)
            .lines()
            .nth(1)
            .unwrap_or_default()
            .to_owned();
        let status = self
            .git(&[
                "status",
                "--porcelain=v2",
                "-z",
                "--branch",
                "--untracked-files=all",
                "--",
                ".",
            ])
            .await
            .and_then(|output| {
                if output.status.success() {
                    Ok(output)
                } else {
                    Err(WorkspaceError::Git(
                        String::from_utf8_lossy(&output.stderr)
                            .trim()
                            .to_owned(),
                    ))
                }
            });
        let output = match status {
            Ok(output) => output,
            Err(error) => {
                tracing::warn!(
                    "git status failed for workspace {}: {}",
                    self.0.display(),
                    error
                );
                return WorkspaceGitStatus::GitUnavailable;
            }
        };
        let mut status = WorkspaceGitStatus::from_porcelain_v2(
            &output.stdout,
            WORKSPACE_CHANGE_LIMIT,
        );
        if let WorkspaceGitStatus::Repository { changes, .. } = &mut status {
            for change in changes {
                Self::strip_prefix(&mut change.path, &prefix);
                if let Some(original) = &mut change.original_path {
                    Self::strip_prefix(original, &prefix);
                }
            }
        }
        if let Err(error) = self.count_lines(&mut status).await {
            // Counts are a summary; the change list stays useful without them.
            tracing::warn!(
                "failed to count changed lines in workspace {}: {}",
                self.0.display(),
                error
            );
        }
        status
    }

    /// Fills the diff base and per-file line counts of a repository status.
    ///
    /// Tracked files come from `git diff --numstat` against the base;
    /// untracked files count every line as added, read off the async workers.
    pub(super) async fn count_lines(
        &self,
        status: &mut WorkspaceGitStatus,
    ) -> Result<(), WorkspaceError> {
        let base = self.diff_base().await?;
        let numstat = self
            .git(&[
                "diff",
                "--numstat",
                "-z",
                "--relative",
                "--no-ext-diff",
                base.rev.as_str(),
            ])
            .await?;
        if !numstat.status.success() {
            return Err(Self::failure(&numstat.stderr));
        }
        status.apply_numstat(&numstat.stdout);
        let WorkspaceGitStatus::Repository {
            base: label,
            changes,
            insertions,
            ..
        } = status
        else {
            return Ok(());
        };
        *label = base.label;
        let untracked: Vec<(usize, PathBuf)> = changes
            .iter()
            .enumerate()
            .filter(|(_, change)| change.kind == WorkspaceChangeKind::Untracked)
            .map(|(index, change)| (index, self.0.join(&change.path)))
            .collect();
        let counted = tokio::task::spawn_blocking(move || {
            untracked
                .into_iter()
                .map(|(index, path)| (index, count_file_lines(&path)))
                .collect::<Vec<_>>()
        })
        .await
        .map_err(|error| WorkspaceError::Io(error.to_string()))?;
        for (index, lines) in counted {
            if let Some(change) = changes.get_mut(index) {
                change.insertions = lines;
                change.deletions = lines.map(|_| 0);
                *insertions += lines.unwrap_or(0);
            }
        }
        Ok(())
    }

    /// Removes the workspace's prefix inside its repository from a Git path.
    pub(super) fn strip_prefix(path: &mut String, prefix: &str) {
        if let Some(stripped) = path.strip_prefix(prefix) {
            *path = stripped.to_owned();
        }
    }
}
