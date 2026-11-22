//! Per-run workspace snapshots kept as hidden Git refs (M4b spec §5).
//!
//! A snapshot is a commit of the whole work tree built in a scratch index, so
//! the user's index, HEAD, branch, and files stay as they were. Each run has
//! two, `refs/clawcode/checkpoints/<session>/<run>/start` and `…/end`; the
//! change a run made is the diff between them. The end snapshot's message
//! records that change once, so listing a session's runs reads only refs.

use std::collections::{BTreeMap, HashMap};
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::Output;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use kernel::{CheckpointPoint, CheckpointStage, RunCheckpoints};
use protocol::{
    CheckpointList, MessageId, RunCheckpoint, RunId, SessionId,
    WORKSPACE_CHANGE_LIMIT, WorkspaceChanges, WorkspaceDiff,
};
use serde::{Deserialize, Serialize};

use super::{WorkspaceError, WorkspaceRoot};

/// Where every snapshot ref lives.
const REF_ROOT: &str = "refs/clawcode/checkpoints";
/// Prefix of the scratch files, next to the repository's index.
const SCRATCH_PREFIX: &str = "clawcode-checkpoint-index-";
/// Author and committer of snapshot commits, whatever the user configured.
const IDENTITY: [(&str, &str); 4] = [
    ("GIT_AUTHOR_NAME", "clawcode"),
    ("GIT_AUTHOR_EMAIL", "clawcode@localhost"),
    ("GIT_COMMITTER_NAME", "clawcode"),
    ("GIT_COMMITTER_EMAIL", "clawcode@localhost"),
];

/// Distinguishes scratch files of concurrent snapshots in one process.
static SCRATCH_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// What a snapshot commit's message records about its run.
#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SnapshotNote {
    /// The run the snapshot belongs to.
    run_id: Option<RunId>,
    /// When a start snapshot was taken, as Unix milliseconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    started_at_ms: Option<u64>,
    /// The run's last reply, recorded by its end snapshot.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    anchor_message_id: Option<MessageId>,
    /// What the run changed within the workspace, recorded by its end
    /// snapshot: snapshots never change, so it is worked out once.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    changes: Option<WorkspaceChanges>,
}

/// The two snapshot notes of one run, as far as they exist.
#[derive(Default)]
struct RunNotes {
    start: Option<SnapshotNote>,
    end: Option<SnapshotNote>,
}

/// A snapshot's scratch index and commit message, deleted when the
/// snapshot is done, however it ends.
struct Scratch {
    index: PathBuf,
}

impl Scratch {
    /// Scratch files beside the repository's `index`, unique per snapshot.
    fn beside(index: &Path) -> Self {
        Self {
            index: index.with_file_name(format!(
                "{SCRATCH_PREFIX}{}-{}",
                std::process::id(),
                SCRATCH_SEQUENCE.fetch_add(1, Ordering::Relaxed)
            )),
        }
    }

    /// The scratch path with `suffix` appended.
    fn with_suffix(&self, suffix: &str) -> PathBuf {
        let mut path = self.index.clone().into_os_string();
        path.push(suffix);
        PathBuf::from(path)
    }

    /// The file holding the commit message: a run's change list can pass
    /// the 128 KiB limit of one command-line argument.
    fn message(&self) -> PathBuf {
        self.with_suffix(".msg")
    }
}

impl Drop for Scratch {
    /// Removes the index, its message, and the lock git keeps beside the
    /// index while writing, which a git process killed at a time limit
    /// leaves behind; any may never have been written.
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.index);
        let _ = std::fs::remove_file(self.message());
        let _ = std::fs::remove_file(self.with_suffix(".lock"));
    }
}

/// Takes and removes snapshots for the Kernel; snapshots of one repository
/// are taken one at a time.
#[derive(Default)]
pub struct GitCheckpoints {
    /// One lock per repository, by its common Git directory.
    locks: Mutex<HashMap<PathBuf, Arc<tokio::sync::Mutex<()>>>>,
}

impl GitCheckpoints {
    /// The lock of the repository whose common Git directory is `common`.
    fn lock(&self, common: PathBuf) -> Arc<tokio::sync::Mutex<()>> {
        let mut locks = self.locks.lock().unwrap_or_else(|poisoned| {
            // A panicked holder left only the map behind, which is still valid.
            poisoned.into_inner()
        });
        Arc::clone(locks.entry(common).or_default())
    }
}

#[async_trait::async_trait]
impl RunCheckpoints for GitCheckpoints {
    /// Snapshots the workspace of `point` under its run's ref; a workspace
    /// outside Git is skipped.
    async fn capture(&self, point: CheckpointPoint<'_>) -> Result<(), String> {
        let root = WorkspaceRoot::try_from(point.cwd)
            .map_err(|error| error.to_string())?;
        let Some(paths) =
            root.git_paths().await.map_err(|error| error.to_string())?
        else {
            return Ok(());
        };
        let lock = self.lock(paths.common.clone());
        let _held = lock.lock().await;
        root.capture_checkpoint(&paths, &point)
            .await
            .map_err(|error| error.to_string())
    }

    /// Deletes every snapshot ref of `session_id`.
    async fn remove(
        &self,
        cwd: &Path,
        session_id: &SessionId,
    ) -> Result<(), String> {
        let root =
            WorkspaceRoot::try_from(cwd).map_err(|error| error.to_string())?;
        root.remove_checkpoints(session_id)
            .await
            .map_err(|error| error.to_string())
    }
}

/// The current time as Unix milliseconds.
fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| {
            u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX)
        })
}

/// The ref of one snapshot of `run` in `session`.
fn snapshot_ref(session: &SessionId, run: &RunId, stage: &str) -> String {
    format!("{REF_ROOT}/{session}/{run}/{stage}")
}

/// Where a repository keeps what snapshots need.
struct GitPaths {
    /// The common Git directory, shared by linked worktrees.
    common: PathBuf,
    /// The index of this work tree.
    index: PathBuf,
}

impl WorkspaceRoot {
    /// The repository's common Git directory and index, absolute; `None`
    /// outside Git. One `git` run answers both.
    async fn git_paths(&self) -> Result<Option<GitPaths>, WorkspaceError> {
        let output = self
            .git(&[
                "rev-parse",
                "--path-format=absolute",
                "--git-common-dir",
                "--git-path",
                "index",
            ])
            .await?;
        if !output.status.success() {
            return Ok(None);
        }
        let text = String::from_utf8_lossy(&output.stdout);
        let mut lines = text.lines().map(PathBuf::from);
        Ok(lines
            .next()
            .zip(lines.next())
            .map(|(common, index)| GitPaths { common, index }))
    }

    /// Commits the whole work tree, as `git add -A` sees it, to the ref of
    /// `point`, through a scratch copy of the index. An end snapshot records
    /// the run's change in its message, and is not written when the run
    /// changed nothing within this workspace.
    async fn capture_checkpoint(
        &self,
        paths: &GitPaths,
        point: &CheckpointPoint<'_>,
    ) -> Result<(), WorkspaceError> {
        let scratch = Scratch::beside(&paths.index);
        // A copy keeps the index's stat cache, so `add -A` rehashes only
        // files that changed; a repository without an index starts empty.
        match tokio::fs::copy(&paths.index, &scratch.index).await {
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(WorkspaceError::Io(error.to_string())),
        }
        // ponytail: `add -A` hashes every new untracked file, however large; skip
        // files over a size limit, as T3 caps untracked bytes, if that gets slow.
        let scratch_env = [("GIT_INDEX_FILE", scratch.index.as_os_str())];
        Self::succeeded(
            self.git_with_env(&["add", "-A"], &scratch_env).await?,
        )?;
        let tree = Self::succeeded(
            self.git_with_env(&["write-tree"], &scratch_env).await?,
        )?;
        let note = match point.stage {
            CheckpointStage::Start => SnapshotNote {
                run_id: Some(point.run_id.clone()),
                started_at_ms: Some(now_ms()),
                ..SnapshotNote::default()
            },
            CheckpointStage::End => {
                let start =
                    snapshot_ref(point.session_id, point.run_id, "start");
                let changes = self.changes_between(&start, &tree).await?;
                if changes.changes.is_empty() {
                    return Ok(());
                }
                SnapshotNote {
                    run_id: Some(point.run_id.clone()),
                    anchor_message_id: point.anchor.cloned(),
                    changes: Some(changes),
                    ..SnapshotNote::default()
                }
            }
        };
        let message = serde_json::to_vec(&note)
            .map_err(|error| WorkspaceError::Io(error.to_string()))?;
        tokio::fs::write(scratch.message(), message)
            .await
            .map_err(|error| WorkspaceError::Io(error.to_string()))?;
        let identity = IDENTITY.map(|(key, value)| (key, OsStr::new(value)));
        let message_path = scratch.message();
        let commit = Self::succeeded(
            self.git_with_env(
                &[
                    OsStr::new("commit-tree"),
                    OsStr::new(&tree),
                    OsStr::new("-F"),
                    message_path.as_os_str(),
                ],
                &identity,
            )
            .await?,
        )?;
        let stage = match point.stage {
            CheckpointStage::Start => "start",
            CheckpointStage::End => "end",
        };
        let refname = snapshot_ref(point.session_id, point.run_id, stage);
        Self::succeeded(
            self.git(&["update-ref", refname.as_str(), commit.as_str()])
                .await?,
        )?;
        Ok(())
    }

    /// The files changed between two tree-ishes within this workspace.
    async fn changes_between(
        &self,
        from: &str,
        to: &str,
    ) -> Result<WorkspaceChanges, WorkspaceError> {
        let diff = Self::succeeded_bytes(
            self.git(&[
                "diff",
                "--raw",
                "--numstat",
                "-z",
                "-M",
                "--relative",
                "--no-ext-diff",
                from,
                to,
            ])
            .await?,
        )?;
        Ok(WorkspaceChanges::from_raw_numstat(
            &diff,
            WORKSPACE_CHANGE_LIMIT,
        ))
    }

    /// The runs of `session` that changed this workspace, oldest first, as
    /// their end snapshots recorded them; outside Git the list is empty.
    pub async fn checkpoints(
        &self,
        session: &SessionId,
    ) -> Result<CheckpointList, WorkspaceError> {
        let mut runs: Vec<RunCheckpoint> = self
            .snapshot_notes(session)
            .await?
            .into_iter()
            .filter_map(|(run, notes)| {
                let end = notes.end?;
                Some(
                    RunCheckpoint::builder()
                        .run_id(run)
                        .anchor_message_id(end.anchor_message_id)
                        .started_at_ms(
                            notes
                                .start
                                .and_then(|start| start.started_at_ms)
                                .unwrap_or_default(),
                        )
                        .changes(end.changes?)
                        .build(),
                )
            })
            .collect();
        runs.sort_by(|a, b| {
            (a.started_at_ms, a.run_id.as_ref())
                .cmp(&(b.started_at_ms, b.run_id.as_ref()))
        });
        Ok(CheckpointList { runs })
    }

    /// One file's change between the snapshots of `run` in `session`; a
    /// renamed file names its `original` path too, or git shows it as new.
    pub async fn checkpoint_diff(
        &self,
        session: &SessionId,
        run: &RunId,
        relative: impl AsRef<Path>,
        original: Option<&str>,
    ) -> Result<WorkspaceDiff, WorkspaceError> {
        let path = Self::checked(relative.as_ref())?;
        let original = original.map(Self::checked::<str>).transpose()?;
        let start = snapshot_ref(session, run, "start");
        let end = snapshot_ref(session, run, "end");
        let mut args = vec![
            OsStr::new("diff"),
            OsStr::new("--no-color"),
            OsStr::new("--no-ext-diff"),
            OsStr::new("--relative"),
            OsStr::new("-M"),
            OsStr::new(&start),
            OsStr::new(&end),
            OsStr::new("--"),
        ];
        args.extend(original.map(Path::as_os_str));
        args.push(path.as_os_str());
        let output = self.git_limited(&args).await?;
        output.check()?;
        Ok(WorkspaceDiff::from_git_output(
            output.stdout,
            output.over_limit,
        ))
    }

    /// Deletes every snapshot ref of `session`; nothing to do outside Git.
    pub async fn remove_checkpoints(
        &self,
        session: &SessionId,
    ) -> Result<(), WorkspaceError> {
        let prefix = format!("{REF_ROOT}/{session}/");
        let listed = self
            .git(&["for-each-ref", "--format=%(refname)", prefix.as_str()])
            .await?;
        if !listed.status.success() {
            return Ok(());
        }
        for refname in String::from_utf8_lossy(&listed.stdout).lines() {
            Self::succeeded(self.git(&["update-ref", "-d", refname]).await?)?;
        }
        Ok(())
    }

    /// The snapshot notes of `session`, grouped by run; empty outside Git.
    async fn snapshot_notes(
        &self,
        session: &SessionId,
    ) -> Result<BTreeMap<RunId, RunNotes>, WorkspaceError> {
        let prefix = format!("{REF_ROOT}/{session}/");
        let listed = self
            .git(&[
                "for-each-ref",
                "--format=%(refname)%00%(contents)%00",
                prefix.as_str(),
            ])
            .await?;
        let mut runs: BTreeMap<RunId, RunNotes> = BTreeMap::new();
        if !listed.status.success() {
            return Ok(runs);
        }
        let text = String::from_utf8_lossy(&listed.stdout);
        let fields: Vec<&str> = text.split('\0').collect();
        // Each ref is two NUL-ended fields; git ends every record with a
        // newline, which therefore leads the next record's refname.
        for [refname, contents] in fields.as_chunks::<2>().0 {
            let Some(rest) = refname
                .trim_start_matches('\n')
                .strip_prefix(prefix.as_str())
            else {
                continue;
            };
            let Some((run, stage)) = rest.split_once('/') else {
                continue;
            };
            let Ok(run) = RunId::try_from(run) else {
                continue;
            };
            let note =
                serde_json::from_str(contents.trim()).unwrap_or_default();
            let entry = runs.entry(run).or_default();
            match stage {
                "start" => entry.start = Some(note),
                "end" => entry.end = Some(note),
                _ => {}
            }
        }
        Ok(runs)
    }

    /// Runs `git` like [`Self::git`] with extra environment variables.
    async fn git_with_env<S: AsRef<OsStr>>(
        &self,
        args: &[S],
        env: &[(&str, &OsStr)],
    ) -> Result<Output, WorkspaceError> {
        let mut command = self.git_command(args);
        command.envs(env.iter().copied());
        match tokio::time::timeout(super::git::GIT_TIMEOUT, command.output())
            .await
        {
            Ok(Ok(output)) => Ok(output),
            Ok(Err(error)) => Err(WorkspaceError::Git(error.to_string())),
            Err(_elapsed) => Err(Self::timed_out()),
        }
    }

    /// The trimmed stdout of a successful run, or its error.
    fn succeeded(output: Output) -> Result<String, WorkspaceError> {
        Self::succeeded_bytes(output).map(|stdout| Self::trimmed(&stdout))
    }

    /// The stdout of a successful run, or its error.
    fn succeeded_bytes(output: Output) -> Result<Vec<u8>, WorkspaceError> {
        if output.status.success() {
            Ok(output.stdout)
        } else {
            Err(Self::failure(&output.stderr))
        }
    }
}
