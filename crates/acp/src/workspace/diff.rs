//! Patches of the workspace against its diff base or the index.

use std::ffi::OsStr;
use std::path::Path;

use protocol::{WorkspaceDiff, WorkspaceDiffScope};

use super::git::LimitedOutput;
use super::{WorkspaceError, WorkspaceRoot};

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

    /// The diff of one file; a renamed file names its `original` path too,
    /// or git shows it as new.
    ///
    /// `Base` compares the work tree with the diff base, `Unstaged` with the
    /// index. An untracked file is shown as added in both scopes; a deleted
    /// file is included. The patch stops at [`protocol::WORKSPACE_DIFF_LIMIT`].
    pub async fn git_diff(
        &self,
        relative: impl AsRef<Path>,
        original: Option<&str>,
        scope: WorkspaceDiffScope,
    ) -> Result<WorkspaceDiff, WorkspaceError> {
        let rev = self.scope_rev(scope).await?;
        self.file_diff(relative.as_ref(), original, rev.as_deref())
            .await
    }

    /// The revision `scope` compares the work tree with; `None` is the index.
    pub(super) async fn scope_rev(
        &self,
        scope: WorkspaceDiffScope,
    ) -> Result<Option<String>, WorkspaceError> {
        Ok(match scope {
            WorkspaceDiffScope::Base => Some(self.diff_base().await?.rev),
            WorkspaceDiffScope::Unstaged => None,
        })
    }

    /// One file's diff; an untracked file is diffed against `/dev/null`.
    pub(super) async fn file_diff(
        &self,
        relative: &Path,
        original: Option<&str>,
        rev: Option<&str>,
    ) -> Result<WorkspaceDiff, WorkspaceError> {
        let path = Self::checked(relative)?;
        let original = original.map(Self::checked::<str>).transpose()?;
        // A deleted file no longer resolves; its path was checked lexically
        // above. Resolving still refuses a link that leads outside the root.
        let exists = match self.resolve(relative) {
            Ok(_) => true,
            Err(WorkspaceError::NotFound) => false,
            Err(error) => return Err(error),
        };
        let path = path.as_os_str();
        // Only a file git does not track at all is diffed against nothing; a
        // deleted one, staged or committed, diffs against the compared
        // version like any other change.
        let untracked = exists
            && !self
                .git(&[
                    OsStr::new("ls-files"),
                    OsStr::new("--others"),
                    OsStr::new("--exclude-standard"),
                    OsStr::new("--"),
                    path,
                ])
                .await?
                .stdout
                .is_empty();
        let output = if untracked {
            self.untracked_diff(path).await?
        } else {
            let mut args = Self::diff_args(rev);
            // With its old path in the pathspec, git can pair a rename.
            if let Some(original) = original {
                args.extend([
                    OsStr::new("-M"),
                    OsStr::new("--"),
                    original.as_os_str(),
                ]);
                args.push(path);
            } else {
                args.extend([OsStr::new("--"), path]);
            }
            self.git_limited(&args).await?
        };
        output.check()?;
        Ok(WorkspaceDiff::from_git_output(
            output.stdout,
            output.over_limit,
        ))
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
}
