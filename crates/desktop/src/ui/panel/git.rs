//! Each registered workspace's last Git status: the branch on its row and
//! the changed-lines summary. Read again when a run in the workspace ends
//! and on refresh; there is no file watching.

use std::path::{Path, PathBuf};

use gpui_kit::Context;
use protocol::WorkspaceGitStatus;

use crate::backend::SessionId;
use crate::ui::AppView;

/// The last Git status read for one workspace.
#[derive(Debug, Clone, Default)]
pub struct GitSnapshot {
    /// The status, or why it could not be read; `None` before the first answer.
    pub status: Option<Result<WorkspaceGitStatus, String>>,
    /// Whether a read is in flight.
    pub loading: bool,
}

impl AppView {
    /// Reads a workspace's Git status again; a read already in flight is not repeated.
    pub fn refresh_git(
        &mut self,
        workspace: PathBuf,
        cx: &mut Context<'_, Self>,
    ) {
        let Some(client) = self.host.client.clone() else {
            return;
        };
        let snapshot = self.git.entry(workspace.clone()).or_default();
        if snapshot.loading {
            return;
        }
        snapshot.loading = true;
        cx.spawn(async move |this, cx| {
            let status = client
                .git_status(&workspace)
                .await
                .map_err(|error| error.to_string());
            let _ = this.update(cx, |this, cx| {
                let snapshot = this.git.entry(workspace).or_default();
                snapshot.loading = false;
                snapshot.status = Some(status);
                cx.notify();
            });
        })
        .detach();
    }

    /// Reads every registered workspace's status, for the branches on their rows.
    pub(in crate::ui) fn refresh_all_git(
        &mut self,
        cx: &mut Context<'_, Self>,
    ) {
        let workspaces: Vec<PathBuf> = self
            .state
            .workspaces
            .entries()
            .iter()
            .map(|entry| entry.path.clone())
            .filter(|path| !self.missing.contains(path))
            .collect();
        for workspace in workspaces {
            self.refresh_git(workspace, cx);
        }
    }

    /// A run of this session ended or its title changed: its workspace's files may have changed.
    pub(in crate::ui) fn refresh_session_git(
        &mut self,
        id: &SessionId,
        cx: &mut Context<'_, Self>,
    ) {
        let Some(cwd) =
            self.host.catalog.get(id).map(|summary| summary.cwd.clone())
        else {
            return;
        };
        if self.state.workspaces.contains(&cwd) {
            self.reload_changes(&cwd, cx);
            self.refresh_git(cwd, cx);
        }
    }

    /// Number of changed files in a workspace, as last read.
    pub fn change_count(&self, workspace: &Path) -> usize {
        match self.git.get(workspace).and_then(|git| git.status.as_ref()) {
            Some(Ok(WorkspaceGitStatus::Repository { changes, .. })) => {
                changes.len()
            }
            _ => 0,
        }
    }

    /// The checked-out branch of a workspace, as last read.
    pub(in crate::ui) fn branch(&self, workspace: &Path) -> Option<String> {
        match self.git.get(workspace)?.status.as_ref()? {
            Ok(WorkspaceGitStatus::Repository { branch, .. }) => branch.clone(),
            _ => None,
        }
    }

    /// A workspace's last read Git status, when it was read successfully.
    pub(in crate::ui) fn git_status(
        &self,
        workspace: &Path,
    ) -> Option<&protocol::WorkspaceGitStatus> {
        self.git.get(workspace)?.status.as_ref()?.as_ref().ok()
    }

    /// Whether a workspace is a Git work tree, as last read; `None` before the first read.
    pub fn is_repository(&self, workspace: &Path) -> Option<bool> {
        match self.git.get(workspace)?.status.as_ref()? {
            Ok(protocol::WorkspaceGitStatus::Repository { .. }) => Some(true),
            Ok(_) => Some(false),
            Err(_) => None,
        }
    }
}
