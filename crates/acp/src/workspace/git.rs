//! Running `git` for the workspace tools: time limits, output limits, and errors.

use std::ffi::OsStr;
use std::process::{Output, Stdio};
use std::time::Duration;

use protocol::WORKSPACE_DIFF_LIMIT;
use tokio::io::AsyncReadExt as _;

use super::{WorkspaceError, WorkspaceRoot};

/// Longest one `git` command may run.
pub(super) const GIT_TIMEOUT: Duration = Duration::from_secs(10);
/// Bytes of a `git` run's stderr kept for its error message.
const STDERR_KEPT: usize = 64 * 1024;

/// Output of a `git` run whose stdout was read up to a byte limit.
#[derive(typed_builder::TypedBuilder)]
pub(super) struct LimitedOutput {
    /// Stdout, at most the limit plus one byte.
    pub(super) stdout: Vec<u8>,
    /// Whether stdout went past the limit (the process was stopped then).
    pub(super) over_limit: bool,
    /// The exit code; `None` when the process was stopped or killed by a signal.
    #[builder(default)]
    pub(super) code: Option<i32>,
    /// Stderr, for error messages.
    pub(super) stderr: Vec<u8>,
}

impl LimitedOutput {
    /// Fails unless `git diff` exited with 0 (same) or 1 (`--no-index` found a
    /// difference), or was stopped at the size limit.
    pub(super) fn check(&self) -> Result<(), WorkspaceError> {
        if self.over_limit || matches!(self.code, Some(0 | 1)) {
            Ok(())
        } else {
            Err(WorkspaceRoot::failure(&self.stderr))
        }
    }
}

impl WorkspaceRoot {
    /// Runs `git` like [`Self::git`] but reads at most [`WORKSPACE_DIFF_LIMIT`]
    /// plus one byte of stdout, then stops the process, so a huge diff is
    /// never held in memory whole.
    pub(super) async fn git_limited<S: AsRef<OsStr>>(
        &self,
        args: &[S],
    ) -> Result<LimitedOutput, WorkspaceError> {
        let mut command = self.git_command(args);
        command.stdout(Stdio::piped()).stderr(Stdio::piped());
        let run = async {
            let mut child = command
                .spawn()
                .map_err(|error| WorkspaceError::Git(error.to_string()))?;
            // stderr drains on its own task: a full stderr pipe would block git
            // while this reads stdout (e.g. one autocrlf warning per file).
            let stderr = child.stderr.take().map(|mut pipe| {
                tokio::spawn(async move {
                    // Reads to the end, keeping only the start for error messages.
                    let mut kept = Vec::new();
                    let mut buffer = [0u8; 8192];
                    while let Ok(read @ 1..) = pipe.read(&mut buffer).await {
                        let room = STDERR_KEPT.saturating_sub(kept.len());
                        kept.extend_from_slice(
                            buffer.get(..read.min(room)).unwrap_or_default(),
                        );
                    }
                    kept
                })
            });
            let mut stdout = Vec::new();
            if let Some(pipe) = child.stdout.take() {
                pipe.take(WORKSPACE_DIFF_LIMIT as u64 + 1)
                    .read_to_end(&mut stdout)
                    .await
                    .map_err(|error| WorkspaceError::Io(error.to_string()))?;
            }
            let over_limit = stdout.len() > WORKSPACE_DIFF_LIMIT;
            if over_limit {
                // The rest of the output is not wanted; stop producing it.
                let _ = child.start_kill();
            }
            let status = child
                .wait()
                .await
                .map_err(|error| WorkspaceError::Git(error.to_string()))?;
            let stderr = match stderr {
                Some(task) => task.await.unwrap_or_default(),
                None => Vec::new(),
            };
            Ok(LimitedOutput::builder()
                .stdout(stdout)
                .over_limit(over_limit)
                .code(if over_limit { None } else { status.code() })
                .stderr(stderr)
                .build())
        };
        tokio::time::timeout(GIT_TIMEOUT, run)
            .await
            .unwrap_or_else(|_elapsed| Err(Self::timed_out()))
    }

    /// Runs `git -C <root>` with `args`, without taking optional locks and with a time limit.
    pub(super) async fn git<S: AsRef<OsStr>>(
        &self,
        args: &[S],
    ) -> Result<Output, WorkspaceError> {
        let mut command = self.git_command(args);
        match tokio::time::timeout(GIT_TIMEOUT, command.output()).await {
            Ok(Ok(output)) => Ok(output),
            Ok(Err(error)) => Err(WorkspaceError::Git(error.to_string())),
            Err(_elapsed) => Err(Self::timed_out()),
        }
    }

    /// A `git -C <root>` command that takes no optional locks and reads client paths literally.
    pub(super) fn git_command<S: AsRef<OsStr>>(
        &self,
        args: &[S],
    ) -> tokio::process::Command {
        let mut command = tokio::process::Command::new("git");
        command
            .arg("-C")
            .arg(&self.0)
            .args(args)
            .env("GIT_OPTIONAL_LOCKS", "0")
            // Paths come from the client: `[id]` or `:x` name that file, not a glob or magic pathspec.
            .env("GIT_LITERAL_PATHSPECS", "1")
            .stdin(Stdio::null())
            .kill_on_drop(true);
        command
    }

    /// The error of a `git` run that passed [`GIT_TIMEOUT`].
    pub(super) fn timed_out() -> WorkspaceError {
        WorkspaceError::Git(format!(
            "git did not finish within {} s",
            GIT_TIMEOUT.as_secs()
        ))
    }

    /// A failed `git` run's error, from its stderr.
    pub(super) fn failure(stderr: &[u8]) -> WorkspaceError {
        WorkspaceError::Git(String::from_utf8_lossy(stderr).trim().to_owned())
    }

    /// One line of `git` output without its trailing newline.
    pub(super) fn trimmed(stdout: &[u8]) -> String {
        String::from_utf8_lossy(stdout).trim().to_owned()
    }
}
