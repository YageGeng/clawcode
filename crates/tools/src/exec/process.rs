//! One exec child: a PTY or plain pipes, its output, exit, and process group.

use std::collections::BTreeMap;
use std::io::{Read as _, Write as _};
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncReadExt as _};
use tokio::sync::watch;
use tokio::task::JoinSet;

use super::{ExecError, OUTPUT_MAX_BYTES, OutputBuffers};

/// After exit, output still in flight is read for at most this long, so a
/// detached grandchild holding the pipes cannot keep the process unfinished.
const EXIT_OUTPUT_GRACE: Duration = Duration::from_millis(500);
/// Bytes read per output chunk.
const READ_CHUNK_BYTES: usize = 8 * 1024;
/// PTY size, as in codex.
const PTY_ROWS: u16 = 24;
/// PTY width, as in codex.
const PTY_COLS: u16 = 80;

/// Everything needed to start one child.
#[derive(Debug, Clone, typed_builder::TypedBuilder)]
pub struct SpawnSpec {
    /// Program followed by its arguments.
    pub argv: Vec<String>,
    /// Existing working directory.
    pub cwd: PathBuf,
    /// Complete child environment; nothing else is inherited.
    pub env: BTreeMap<String, String>,
    /// Whether to run under a PTY with a writable stdin.
    pub tty: bool,
}

/// Observable lifecycle of one child.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProcessState {
    /// The child process has exited.
    pub exited: bool,
    /// Exit code, when the child exited normally.
    pub exit_code: Option<i32>,
    /// Signal name, such as `SIGKILL`, when a signal ended the child.
    pub signal: Option<String>,
    /// No more output will be appended.
    pub output_closed: bool,
}

impl ProcessState {
    /// Exited and fully read: nothing about this child changes any more.
    #[must_use]
    pub const fn finished(&self) -> bool {
        self.exited && self.output_closed
    }
}

/// Output and state shared between the child's I/O tasks and its owner.
#[derive(Clone)]
struct ProcessIo {
    buffers: Arc<Mutex<OutputBuffers>>,
    state: Arc<watch::Sender<ProcessState>>,
    /// The group had no members left once the leader was reaped, so its id
    /// may already name an unrelated group and must never be signalled.
    group_empty: Arc<AtomicBool>,
}

impl ProcessIo {
    /// Appends one chunk to both output views.
    fn push(&self, chunk: &[u8]) {
        self.buffers
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push_chunk(chunk);
    }

    /// Records how the child exited.
    fn mark_exit(&self, exit_code: Option<i32>, signal: Option<String>) {
        self.state.send_modify(|state| {
            state.exited = true;
            state.exit_code = exit_code;
            state.signal = signal;
        });
    }

    /// Probes the reaped leader's group: an empty group frees the id for
    /// reuse, so later signals are skipped; a group with members (detached
    /// daemons) keeps its id reserved and can still be killed safely.
    fn note_group_after_exit(&self, pid: Option<u32>) {
        #[cfg(unix)]
        if let Some(pid) = pid.and_then(|pid| i32::try_from(pid).ok()) {
            // shortcut: members that exit later can still free the id before a terminate; use waitid(WNOWAIT) to kill before reaping if that window matters.
            let probe =
                nix::sys::signal::killpg(nix::unistd::Pid::from_raw(pid), None);
            if probe == Err(nix::errno::Errno::ESRCH) {
                self.group_empty.store(true, Ordering::Release);
            }
        }
        #[cfg(not(unix))]
        let _unused = pid;
    }

    /// Records that no more output will arrive.
    fn close_output(&self) {
        self.state.send_modify(|state| state.output_closed = true);
    }

    /// Reads one async stream into the buffers until it ends.
    async fn read_stream(self, mut stream: impl AsyncRead + Unpin) {
        let mut chunk = vec![0_u8; READ_CHUNK_BYTES];
        loop {
            match stream.read(&mut chunk).await {
                Ok(0) | Err(_) => break,
                Ok(read) => self.push(chunk.get(..read).unwrap_or_default()),
            }
        }
    }
}

/// Transport-specific handle used for input and Windows termination.
enum Handle {
    /// Plain pipes; stdin is closed.
    Pipe,
    /// A PTY whose master receives input.
    Pty {
        /// Input queued for this child's writer thread.
        input: tokio::sync::mpsc::UnboundedSender<Vec<u8>>,
        #[cfg_attr(unix, allow(dead_code))]
        killer: Mutex<Box<dyn portable_pty::ChildKiller + Send + Sync>>,
        /// Held so the PTY stays open; `Mutex` makes the handle `Sync`.
        _master: Mutex<Box<dyn portable_pty::MasterPty + Send>>,
    },
}

/// A running or finished exec child; dropping it kills its process group.
pub struct ExecProcess {
    handle: Handle,
    pid: Option<u32>,
    io: ProcessIo,
    interaction_lock: Arc<tokio::sync::Mutex<()>>,
}

impl ExecProcess {
    /// Starts the child and its I/O tasks; must run inside a Tokio runtime.
    pub fn spawn(spec: SpawnSpec) -> Result<Self, ExecError> {
        let io = ProcessIo {
            buffers: Arc::new(Mutex::new(OutputBuffers::new(OUTPUT_MAX_BYTES))),
            state: Arc::new(watch::Sender::new(ProcessState::default())),
            group_empty: Arc::new(AtomicBool::new(false)),
        };
        let (handle, pid) = if spec.tty {
            Self::spawn_pty(&spec, &io)?
        } else {
            Self::spawn_pipe(&spec, &io)?
        };
        Ok(Self {
            handle,
            pid,
            io,
            interaction_lock: Arc::new(tokio::sync::Mutex::new(())),
        })
    }

    /// Starts a pipe child in its own session and drains stdout and stderr.
    fn spawn_pipe(
        spec: &SpawnSpec,
        io: &ProcessIo,
    ) -> Result<(Handle, Option<u32>), ExecError> {
        let (program, args) = spec.argv.split_first().ok_or_else(|| {
            ExecError::CreateProcess("missing command line".to_string())
        })?;
        let mut command = tokio::process::Command::new(program);
        command
            .args(args)
            .current_dir(&spec.cwd)
            .env_clear()
            .envs(&spec.env)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        #[cfg(unix)]
        detach_from_terminal(&mut command);
        let mut child = command
            .spawn()
            .map_err(|error| ExecError::CreateProcess(error.to_string()))?;
        let pid = child.id();
        let mut readers = JoinSet::new();
        if let Some(stdout) = child.stdout.take() {
            readers.spawn(io.clone().read_stream(stdout));
        }
        if let Some(stderr) = child.stderr.take() {
            readers.spawn(io.clone().read_stream(stderr));
        }
        let io = io.clone();
        tokio::spawn(async move {
            let (exit_code, signal) = match child.wait().await {
                Ok(status) => (status.code(), signal_name(status)),
                Err(error) => {
                    tracing::warn!("failed to wait for exec child: {}", error);
                    (None, None)
                }
            };
            io.mark_exit(exit_code, signal);
            // Bounded drain: inherited pipes may outlive the child.
            let drained = tokio::time::timeout(EXIT_OUTPUT_GRACE, async {
                while readers.join_next().await.is_some() {}
            })
            .await;
            if drained.is_err() {
                readers.abort_all();
            }
            io.note_group_after_exit(pid);
            io.close_output();
        });
        Ok((Handle::Pipe, pid))
    }

    /// Starts a PTY child, which portable-pty makes a session leader with the
    /// PTY as its controlling terminal.
    fn spawn_pty(
        spec: &SpawnSpec,
        io: &ProcessIo,
    ) -> Result<(Handle, Option<u32>), ExecError> {
        let (program, args) = spec.argv.split_first().ok_or_else(|| {
            ExecError::CreateProcess("missing command line".to_string())
        })?;
        let pair = portable_pty::native_pty_system()
            .openpty(portable_pty::PtySize {
                rows: PTY_ROWS,
                cols: PTY_COLS,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|error| ExecError::CreateProcess(error.to_string()))?;
        let mut builder = portable_pty::CommandBuilder::new(program);
        builder.args(args);
        builder.cwd(&spec.cwd);
        builder.env_clear();
        for (name, value) in &spec.env {
            builder.env(name, value);
        }
        let child = pair
            .slave
            .spawn_command(builder)
            .map_err(|error| ExecError::CreateProcess(error.to_string()))?;
        // The child holds its own slave; ours would keep reads from ending.
        drop(pair.slave);
        let pid = child.process_id();
        let killer = child.clone_killer();
        let mut reader = pair
            .master
            .try_clone_reader()
            .map_err(|error| ExecError::CreateProcess(error.to_string()))?;
        let mut writer = pair
            .master
            .take_writer()
            .map_err(|error| ExecError::CreateProcess(error.to_string()))?;
        let (input, mut queued) =
            tokio::sync::mpsc::unbounded_channel::<Vec<u8>>();
        // One writer per child keeps input ordered; a child that stops reading
        // blocks only this thread, which ends once the PTY hangs up.
        // shortcut: a Ctrl-C queued behind input the child never reads cannot reach the full PTY buffer, and the queue has no cap (codex behaves the same; `/stop` still kills); signal the terminal's foreground group when ISIG is on and cap the backlog if that shows up.
        tokio::task::spawn_blocking(move || {
            while let Some(bytes) = queued.blocking_recv() {
                if let Err(error) =
                    writer.write_all(&bytes).and_then(|()| writer.flush())
                {
                    tracing::warn!(
                        "stopped writing to exec PTY input: {}",
                        error
                    );
                    break;
                }
            }
        });
        let reader_io = io.clone();
        let reader_task = tokio::task::spawn_blocking(move || {
            let mut chunk = vec![0_u8; READ_CHUNK_BYTES];
            loop {
                match reader.read(&mut chunk) {
                    Ok(0) | Err(_) => break,
                    Ok(read) => {
                        reader_io.push(chunk.get(..read).unwrap_or_default());
                    }
                }
            }
        });
        let io = io.clone();
        tokio::spawn(async move {
            let (exit_code, signal) = PtyExit::wait(child, pid).await;
            io.mark_exit(exit_code, signal);
            // A blocking read cannot be cancelled; stop waiting for it instead.
            let _drained =
                tokio::time::timeout(EXIT_OUTPUT_GRACE, reader_task).await;
            io.note_group_after_exit(pid);
            io.close_output();
        });
        Ok((
            Handle::Pty {
                input,
                killer: Mutex::new(killer),
                _master: Mutex::new(pair.master),
            },
            pid,
        ))
    }

    /// Queues bytes for a PTY child's input, as codex does, so a child that
    /// stops reading never blocks the caller; pipe children have no stdin.
    pub fn write(&self, bytes: &[u8]) -> Result<(), ExecError> {
        let Handle::Pty { input, .. } = &self.handle else {
            return Err(ExecError::StdinClosed);
        };
        input
            .send(bytes.to_vec())
            .map_err(|_closed| ExecError::WriteToStdin)
    }

    /// Sends SIGINT to the child's process group; Windows terminates instead.
    pub fn interrupt(&self) -> Result<(), ExecError> {
        if self.io.group_empty.load(Ordering::Acquire) {
            return Ok(());
        }
        #[cfg(unix)]
        {
            let Some(group) = self.group() else {
                return Err(ExecError::ProcessFailed(
                    "process has no id".to_string(),
                ));
            };
            nix::sys::signal::killpg(group, nix::sys::signal::Signal::SIGINT)
                .map_err(|error| ExecError::ProcessFailed(error.to_string()))
        }
        #[cfg(not(unix))]
        {
            self.terminate();
            Ok(())
        }
    }

    /// Kills the child and every process left in its group.
    pub fn terminate(&self) {
        if self.io.group_empty.load(Ordering::Acquire) {
            return;
        }
        #[cfg(unix)]
        if let Some(group) = self.group() {
            // The group may already be empty; that is the goal, not an error.
            let _result = nix::sys::signal::killpg(
                group,
                nix::sys::signal::Signal::SIGKILL,
            );
        }
        #[cfg(windows)]
        match &self.handle {
            Handle::Pty { killer, .. } => {
                if let Ok(mut killer) = killer.lock() {
                    let _result = killer.kill();
                }
            }
            Handle::Pipe => {
                if let Some(pid) = self.pid {
                    let _status = std::process::Command::new("taskkill")
                        .args(["/F", "/T", "/PID", &pid.to_string()])
                        .stdout(Stdio::null())
                        .stderr(Stdio::null())
                        .status();
                }
            }
        }
    }

    /// The child leads its own session, so its pid is also its group id.
    #[cfg(unix)]
    fn group(&self) -> Option<nix::unistd::Pid> {
        self.pid
            .and_then(|pid| i32::try_from(pid).ok())
            .map(nix::unistd::Pid::from_raw)
    }

    /// Current lifecycle snapshot.
    #[must_use]
    pub fn state(&self) -> ProcessState {
        self.io.state.borrow().clone()
    }

    /// Receiver notified on every lifecycle change.
    #[must_use]
    pub fn subscribe_state(&self) -> watch::Receiver<ProcessState> {
        self.io.state.subscribe()
    }

    /// Waits until the child exited and its output is fully read.
    pub async fn wait_finished(&self) -> ProcessState {
        let mut receiver = self.subscribe_state();
        match receiver.wait_for(ProcessState::finished).await {
            Ok(state) => state.clone(),
            Err(_closed) => self.state(),
        }
    }

    /// Waits until the child exits, `deadline` passes, or `cancellation` fires;
    /// an exit also waits briefly for the remaining output.
    pub async fn settle(
        &self,
        deadline: tokio::time::Instant,
        cancellation: &tokio_util::sync::CancellationToken,
    ) -> Result<ProcessState, ExecError> {
        let mut receiver = self.subscribe_state();
        tokio::select! {
            _exited = async {
                receiver.wait_for(|state| state.exited).await.is_ok()
            } => {}
            () = tokio::time::sleep_until(deadline) => {}
            () = cancellation.cancelled() => return Err(ExecError::Cancelled),
        }
        if self.state().exited {
            let _drained = tokio::time::timeout(
                EXIT_OUTPUT_GRACE + Duration::from_millis(100),
                self.wait_finished(),
            )
            .await;
        }
        Ok(self.state())
    }

    /// Locks both output views of this child; a panicked holder leaves the
    /// bytes intact, so poisoning is ignored.
    pub fn buffers(&self) -> MutexGuard<'_, OutputBuffers> {
        self.io
            .buffers
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    /// Serializes writes and polls on this child.
    #[must_use]
    pub fn interaction_lock(&self) -> Arc<tokio::sync::Mutex<()>> {
        Arc::clone(&self.interaction_lock)
    }

    /// Whether this child runs under a PTY.
    #[must_use]
    pub const fn is_tty(&self) -> bool {
        matches!(self.handle, Handle::Pty { .. })
    }
}

impl Drop for ExecProcess {
    /// Never leaves a child or its group running past its owner.
    fn drop(&mut self) {
        self.terminate();
    }
}

/// Reaps a PTY child and names the signal that ended it.
struct PtyExit;

impl PtyExit {
    /// Unix reaps with `waitpid` for real signal names; portable-pty reports
    /// only `strsignal` text, which differs between platforms.
    async fn wait(
        child: Box<dyn portable_pty::Child + Send + Sync>,
        pid: Option<u32>,
    ) -> (Option<i32>, Option<String>) {
        let waited = tokio::task::spawn_blocking(move || {
            #[cfg(unix)]
            if let Some(pid) = pid.and_then(|pid| i32::try_from(pid).ok()) {
                // Keep the handle alive until reaped; its drop does not wait.
                let _child = child;
                return Self::waitpid(nix::unistd::Pid::from_raw(pid));
            }
            let mut child = child;
            let _unused = pid;
            match child.wait() {
                Ok(status) => (i32::try_from(status.exit_code()).ok(), None),
                Err(error) => {
                    tracing::warn!("failed to wait for PTY child: {}", error);
                    (None, None)
                }
            }
        })
        .await;
        waited.unwrap_or((None, None))
    }

    /// Blocks until `pid` exits and decodes its status.
    #[cfg(unix)]
    fn waitpid(pid: nix::unistd::Pid) -> (Option<i32>, Option<String>) {
        use nix::sys::wait::WaitStatus;
        loop {
            match nix::sys::wait::waitpid(pid, None) {
                Ok(WaitStatus::Exited(_pid, code)) => {
                    return (Some(code), None);
                }
                Ok(WaitStatus::Signaled(_pid, signal, _core)) => {
                    return (None, Some(signal.as_str().to_string()));
                }
                Ok(_other) => {}
                Err(nix::errno::Errno::EINTR) => {}
                Err(error) => {
                    tracing::warn!(
                        "failed to reap PTY child {}: {}",
                        pid,
                        error
                    );
                    return (None, None);
                }
            }
        }
    }
}

/// Names the signal that ended a pipe child, if one did.
#[cfg(unix)]
fn signal_name(status: std::process::ExitStatus) -> Option<String> {
    use std::os::unix::process::ExitStatusExt as _;
    status
        .signal()
        .and_then(|signal| nix::sys::signal::Signal::try_from(signal).ok())
        .map(|signal| signal.as_str().to_string())
}

/// Other platforms report no signal names.
#[cfg(not(unix))]
fn signal_name(_status: std::process::ExitStatus) -> Option<String> {
    None
}

/// Moves a pipe child into its own session so it has no controlling terminal:
/// opening `/dev/tty` then fails at once instead of stopping the child.
#[cfg(unix)]
#[allow(unsafe_code)]
pub(super) fn detach_from_terminal(command: &mut tokio::process::Command) {
    // shortcut: on macOS a non-tty child that never writes survives a parent SIGKILL as an orphan; record pids and reap them at startup if that becomes a problem.
    // SAFETY: the hook runs in the forked child before exec and calls only
    // async-signal-safe syscalls (setsid, setpgid, prctl) without allocating.
    unsafe {
        command.pre_exec(|| {
            if nix::unistd::setsid().is_err() {
                nix::unistd::setpgid(
                    nix::unistd::Pid::from_raw(0),
                    nix::unistd::Pid::from_raw(0),
                )?;
            }
            #[cfg(target_os = "linux")]
            nix::sys::prctl::set_pdeathsig(nix::sys::signal::Signal::SIGTERM)?;
            Ok(())
        });
    }
}
