//! Login-shell state captured once per Session and replayed by every command,
//! so commands see the user's functions, aliases, options, and exports without
//! paying for a login startup each time. Ported from codex without its
//! credential-broker and sandbox handling.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, PoisonError};
use std::time::{Duration, SystemTime};

use protocol::SessionId;

use super::{ExecEnvironment, ExecError, ExecSettings, ShellKind, UserShell};

/// Delimits the snapshot in the capture output, so text printed by startup
/// files is never mistaken for shell state.
const BEGIN_MARKER: &str = "__CLAWCODE_SNAPSHOT_BEGIN__";
/// End of the snapshot in the capture output.
const END_MARKER: &str = "__CLAWCODE_SNAPSHOT_END__";

/// Variables that describe the capturing shell rather than the user's setup.
const VOLATILE_EXPORTS: &[&str] = &["PWD", "OLDPWD", "SHLVL", "_"];

/// Interactive-only or read-only options that a `-c` shell must not replay.
const SKIPPED_OPTIONS: &[&str] = &[
    "monitor",
    "history",
    "emacs",
    "vi",
    "interactive",
    "login",
    "shinstdin",
    "zle",
    "privileged",
    "restricted",
    "singlecommand",
    "login_shell",
];

/// Time limit for capturing a shell snapshot.
const SNAPSHOT_TIMEOUT: Duration = Duration::from_secs(10);
/// Age after which snapshot files of earlier Sessions are removed.
const SNAPSHOT_RETENTION: Duration = Duration::from_secs(3 * 24 * 60 * 60);

/// One Session's snapshot file and the shell it belongs to.
pub struct ShellSnapshot {
    shell: UserShell,
    path: PathBuf,
}

impl ShellSnapshot {
    /// Names the snapshot of `shell` stored at `path`.
    #[must_use]
    pub const fn new(shell: UserShell, path: PathBuf) -> Self {
        Self { shell, path }
    }

    /// Shell whose state the snapshot holds.
    #[must_use]
    pub const fn shell(&self) -> &UserShell {
        &self.shell
    }

    /// Location of the snapshot file.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Runs the shell as an interactive login shell, records its state, and
    /// keeps the file only when sourcing it back succeeds within `timeout`.
    pub async fn capture(
        &self,
        cwd: &Path,
        env: &BTreeMap<String, String>,
        policy: &ExecEnvironment<'_>,
        timeout: Duration,
    ) -> Result<(), ExecError> {
        let script = self.capture_script().ok_or_else(|| {
            ExecError::ProcessFailed(format!(
                "shell snapshots are not supported for {}",
                self.shell.path.display()
            ))
        })?;
        let raw = self.run(&["-lic", &script], cwd, env, timeout).await?;
        let state = Self::extract(&raw, policy).ok_or_else(|| {
            ExecError::ProcessFailed(
                "shell snapshot markers missing".to_string(),
            )
        })?;
        self.write_private(&state)
            .await
            .map_err(|error| ExecError::ProcessFailed(error.to_string()))?;
        let validate = format!(
            ". '{}'",
            super::shell::single_quoted(&self.path.to_string_lossy())
        );
        if let Err(error) =
            self.run(&["-c", &validate], cwd, env, timeout).await
        {
            let _removed = tokio::fs::remove_file(&self.path).await;
            return Err(error);
        }
        tracing::info!(
            "captured shell snapshot of {} at {}",
            self.shell.path.display(),
            self.path.display()
        );
        Ok(())
    }

    /// Removes snapshot files in `dir` last modified before `max_age` ago.
    pub async fn cleanup_stale(dir: &Path, max_age: Duration) {
        let Ok(mut entries) = tokio::fs::read_dir(dir).await else {
            return;
        };
        let cutoff = SystemTime::now()
            .checked_sub(max_age)
            .unwrap_or(SystemTime::UNIX_EPOCH);
        while let Ok(Some(entry)) = entries.next_entry().await {
            let stale = entry
                .metadata()
                .await
                .and_then(|metadata| metadata.modified())
                .is_ok_and(|modified| modified < cutoff);
            if stale
                && let Err(error) = tokio::fs::remove_file(entry.path()).await
            {
                tracing::debug!(
                    "failed to remove stale shell snapshot {}: {}",
                    entry.path().display(),
                    error
                );
            }
        }
    }

    /// Script printing the shell's re-sourceable state between the markers.
    fn capture_script(&self) -> Option<String> {
        let body = match self.shell.kind {
            // `set +o posix` resets `expand_aliases`, so options precede shopt.
            ShellKind::Bash => {
                "declare -f\nalias -p\nset +o\nshopt -p\nexport -p"
            }
            ShellKind::Zsh => {
                "functions\nsetopt | sed 's/^/setopt /'\nalias -L\nexport -p"
            }
            ShellKind::Sh => "alias\nset +o\nexport -p",
            ShellKind::Other => return None,
        };
        Some(format!(
            "printf '%s\\n' '{BEGIN_MARKER}'\nprintf '%s\\n' 'unalias -a 2>/dev/null || true'\n{body}\nprintf '%s\\n' '{END_MARKER}'"
        ))
    }

    /// Keeps the lines between the markers that a `-c` shell can replay.
    fn extract(raw: &str, policy: &ExecEnvironment<'_>) -> Option<String> {
        let (_before, rest) = raw.split_once(&format!("{BEGIN_MARKER}\n"))?;
        let (state, _after) = rest.split_once(END_MARKER)?;
        Some(
            state
                .lines()
                .filter(|line| !Self::volatile(line))
                // Startup files must not restore what the environment policy removes.
                .filter(|line| {
                    Self::exported_name(line)
                        .is_none_or(|name| policy.keeps(name))
                })
                .map(|line| format!("{line}\n"))
                .collect(),
        )
    }

    /// Name of the variable an `export`/`declare -x` line restores.
    fn exported_name(line: &str) -> Option<&str> {
        let rest = line
            .strip_prefix("declare -x ")
            .or_else(|| line.strip_prefix("export "))?;
        Some(rest.split('=').next().unwrap_or(rest))
    }

    /// Writes the snapshot readable only by its owner: startup files often
    /// export credentials.
    async fn write_private(&self, state: &str) -> std::io::Result<()> {
        use tokio::io::AsyncWriteExt as _;
        if let Some(parent) = self.path.parent() {
            let mut builder = tokio::fs::DirBuilder::new();
            builder.recursive(true);
            #[cfg(unix)]
            builder.mode(0o700);
            builder.create(parent).await?;
        }
        let mut options = tokio::fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        options.mode(0o600);
        let mut file = options.open(&self.path).await?;
        file.write_all(state.as_bytes()).await?;
        file.flush().await
    }

    /// Whether a captured line restores capture-time state that must not leak.
    fn volatile(line: &str) -> bool {
        if let Some(name) = Self::exported_name(line) {
            return VOLATILE_EXPORTS.contains(&name);
        }
        let option = line
            .strip_prefix("set -o ")
            .or_else(|| line.strip_prefix("set +o "))
            .or_else(|| line.strip_prefix("setopt "))
            .or_else(|| line.strip_prefix("shopt -s "))
            .or_else(|| line.strip_prefix("shopt -u "));
        option.is_some_and(|option| SKIPPED_OPTIONS.contains(&option.trim()))
    }

    /// Runs the shell detached from any terminal and returns its stdout.
    async fn run(
        &self,
        args: &[&str],
        cwd: &Path,
        env: &BTreeMap<String, String>,
        timeout: Duration,
    ) -> Result<String, ExecError> {
        let mut command = tokio::process::Command::new(&self.shell.path);
        command
            .args(args)
            .current_dir(cwd)
            .env_clear()
            .envs(env)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        // An interactive shell owning a terminal could stop on job control.
        #[cfg(unix)]
        super::process::detach_from_terminal(&mut command);
        let child = command
            .spawn()
            .map_err(|error| ExecError::ProcessFailed(error.to_string()))?;
        let pid = child.id();
        let output =
            match tokio::time::timeout(timeout, child.wait_with_output()).await
            {
                Ok(output) => output.map_err(|error| {
                    ExecError::ProcessFailed(error.to_string())
                })?,
                Err(_elapsed) => {
                    // The shell leads its own group; startup children die with it.
                    #[cfg(unix)]
                    if let Some(pid) =
                        pid.and_then(|pid| i32::try_from(pid).ok())
                    {
                        let _result = nix::sys::signal::killpg(
                            nix::unistd::Pid::from_raw(pid),
                            nix::sys::signal::Signal::SIGKILL,
                        );
                    }
                    tracing::warn!(
                        "shell snapshot of {} timed out after {} ms",
                        self.shell.path.display(),
                        timeout.as_millis()
                    );
                    return Err(ExecError::ProcessFailed(format!(
                        "shell snapshot timed out after {} ms",
                        timeout.as_millis()
                    )));
                }
            };
        if !output.status.success() {
            return Err(ExecError::ProcessFailed(format!(
                "shell snapshot command exited with {}",
                output.status
            )));
        }
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    }
}

/// Where a Session's shell snapshot stands.
enum SnapshotState {
    /// No command has run yet.
    NotStarted,
    /// Capture runs in the background; commands use the login shell meanwhile.
    Pending {
        /// The capture, stopped before its file is removed.
        task: tokio::task::JoinHandle<()>,
        /// Where the capture writes.
        path: PathBuf,
    },
    /// Commands of this shell replay the snapshot.
    Ready(ShellSnapshot),
    /// Capture failed or is disabled; commands keep using the login shell.
    Unavailable,
}

/// One Session's shell snapshot: captured in the background by its first
/// command, replayed by later ones, and stopped and deleted at close.
pub(super) struct SessionSnapshot {
    settings: Arc<ExecSettings>,
    state: Arc<std::sync::Mutex<SnapshotState>>,
}

impl SessionSnapshot {
    /// Starts with nothing captured.
    pub(super) fn new(settings: Arc<ExecSettings>) -> Self {
        Self {
            settings,
            state: Arc::new(std::sync::Mutex::new(SnapshotState::NotStarted)),
        }
    }

    /// Returns the ready snapshot of `shell`; the first call starts capturing
    /// one in the background, so only later commands replay it.
    pub(super) fn for_shell(
        &self,
        shell: &UserShell,
        env: &BTreeMap<String, String>,
        cwd: &Path,
        session_id: &SessionId,
    ) -> Option<PathBuf> {
        let mut state =
            self.state.lock().unwrap_or_else(PoisonError::into_inner);
        match &*state {
            SnapshotState::Ready(snapshot) if snapshot.shell() == shell => {
                return Some(snapshot.path().to_path_buf());
            }
            SnapshotState::NotStarted => {}
            SnapshotState::Ready(_)
            | SnapshotState::Pending { .. }
            | SnapshotState::Unavailable => {
                return None;
            }
        }
        if !self.settings.config.shell_snapshot || !shell.supports_snapshot() {
            *state = SnapshotState::Unavailable;
            return None;
        }
        let dir = self.settings.snapshot_dir.clone();
        let path = dir.join(format!("{session_id}.sh"));
        let snapshot = ShellSnapshot::new(shell.clone(), path.clone());
        let slot = Arc::clone(&self.state);
        let settings = Arc::clone(&self.settings);
        let env = env.clone();
        let cwd = cwd.to_path_buf();
        // Spawned under the state lock, so the task cannot publish before
        // `Pending` holds its handle.
        let task = tokio::spawn(async move {
            ShellSnapshot::cleanup_stale(&dir, SNAPSHOT_RETENTION).await;
            let policy = ExecEnvironment::new(&settings.config.env);
            let captured = snapshot
                .capture(&cwd, &env, &policy, SNAPSHOT_TIMEOUT)
                .await;
            let next = match captured {
                Ok(()) => SnapshotState::Ready(snapshot),
                Err(error) => {
                    tracing::warn!("shell snapshot unavailable: {}", error);
                    SnapshotState::Unavailable
                }
            };
            let mut state = slot.lock().unwrap_or_else(PoisonError::into_inner);
            // A removal already took `Pending`; it deletes the file instead.
            if matches!(*state, SnapshotState::Pending { .. }) {
                *state = next;
            }
        });
        *state = SnapshotState::Pending { task, path };
        None
    }

    /// Stops a capture still running, then deletes the snapshot file.
    pub(super) async fn remove(&self) {
        let taken = std::mem::replace(
            &mut *self.state.lock().unwrap_or_else(PoisonError::into_inner),
            SnapshotState::Unavailable,
        );
        let path = match taken {
            SnapshotState::Ready(snapshot) => snapshot.path().to_path_buf(),
            SnapshotState::Pending { task, path } => {
                // Dropping the capture kills its shell; the file may exist already.
                task.abort();
                let _stopped = task.await;
                tracing::info!(
                    "stopped shell snapshot capture for {}",
                    path.display()
                );
                path
            }
            SnapshotState::NotStarted | SnapshotState::Unavailable => return,
        };
        if let Err(error) = tokio::fs::remove_file(&path).await
            && error.kind() != std::io::ErrorKind::NotFound
        {
            tracing::warn!(
                "failed to remove shell snapshot {}: {}",
                path.display(),
                error
            );
        }
    }
}
