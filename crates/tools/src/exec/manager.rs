//! Per-Session table of exec processes, ported from codex's process manager.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use protocol::{
    ExecExit, ExecProcessInfo, SessionId, ToolCallId, TruncationDetails, TurnId,
};
use tokio::sync::{Mutex, watch};
use tokio_util::sync::CancellationToken;

use super::output::persist_full_output;
use super::snapshot::SessionSnapshot;
use super::{
    Captured, ExecCommandArgs, ExecEnvironment, ExecError, ExecEventSink,
    ExecOutputUpdate, ExecProcess, ExecResponse, ExecSettings, MAX_PROCESSES,
    ProcessState, SpawnSpec, UserShell, WriteStdinArgs,
};
use crate::{DEFAULT_MAX_BYTES, DEFAULT_MAX_LINES, truncate_tail};

/// Most recently used processes that are never evicted.
const PROTECTED_RECENT: usize = 8;
/// Interval between output snapshots of one process.
const STREAM_INTERVAL: Duration = Duration::from_millis(100);
/// How long closing everything waits for exit reports.
const REPORT_WAIT: Duration = Duration::from_secs(2);
/// One `exec_command` call.
#[derive(typed_builder::TypedBuilder)]
pub struct ExecStart {
    /// Validated call arguments.
    pub args: ExecCommandArgs,
    /// Turn working directory that relative `workdir`s resolve against.
    pub cwd: PathBuf,
    /// The call itself; its id becomes the terminal id.
    pub tool_call_id: ToolCallId,
    /// Turn that owns the call.
    pub turn_id: TurnId,
    /// Session exported to the child.
    pub session_id: SessionId,
    /// Turn cancellation; it ends the wait but never the process.
    pub cancellation: CancellationToken,
    /// Receiver of output snapshots and background exits.
    pub sink: Arc<dyn ExecEventSink>,
}

/// One `write_stdin` call.
pub struct ExecWrite {
    /// Validated call arguments.
    pub args: WriteStdinArgs,
    /// Turn cancellation; it ends the wait but never the process.
    pub cancellation: CancellationToken,
}

/// Who reports a process's exit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    /// The first `exec_command` is still waiting.
    Initial,
    /// The first call returned without the exit; the watcher reports it.
    Background,
    /// The first call's result carried the exit.
    ReturnedExit,
}

/// Marks the end of the first call; dropping it unresolved hands the exit to
/// the watcher, so a cancelled or dropped call never loses the report.
struct InitialCall {
    phase: watch::Sender<Phase>,
}

impl InitialCall {
    /// Records how the first call ended.
    fn finish(&self, phase: Phase) {
        self.phase.send_replace(phase);
    }
}

impl Drop for InitialCall {
    /// Falls back to a background report when the call ended without deciding.
    fn drop(&mut self) {
        self.phase.send_if_modified(|phase| {
            let initial = *phase == Phase::Initial;
            if initial {
                *phase = Phase::Background;
            }
            initial
        });
    }
}

/// One retained process.
struct ProcessEntry {
    process: Arc<ExecProcess>,
    command: String,
    cwd: PathBuf,
    tool_call_id: ToolCallId,
    last_used: Instant,
    phase: watch::Receiver<Phase>,
    reported: watch::Receiver<bool>,
}

/// Retained processes and every id handed out, including ones still spawning.
#[derive(Default)]
struct ProcessStore {
    processes: HashMap<i32, ProcessEntry>,
    reserved: HashSet<i32>,
}

impl ProcessStore {
    /// Removes the eviction candidate, skipping processes busy with a call.
    fn evict(&mut self) -> Option<(i32, ProcessEntry)> {
        let mut meta: Vec<(i32, Instant, bool)> = self
            .processes
            .iter()
            .map(|(id, entry)| {
                (*id, entry.last_used, entry.process.state().exited)
            })
            .collect();
        while let Some(id) = prune_candidate(&meta) {
            let idle = self.processes.get(&id).is_some_and(|entry| {
                entry.process.interaction_lock().try_lock().is_ok()
            });
            if idle {
                self.reserved.remove(&id);
                return self.processes.remove(&id).map(|entry| (id, entry));
            }
            meta.retain(|(candidate, _used, _exited)| *candidate != id);
        }
        None
    }
}

/// Picks the process to evict: never one of the eight most recently used,
/// otherwise the least recently used, preferring exited processes.
#[must_use]
pub fn prune_candidate(meta: &[(i32, Instant, bool)]) -> Option<i32> {
    let mut by_recency = meta.to_vec();
    by_recency.sort_by_key(|(_id, used, _exited)| std::cmp::Reverse(*used));
    let protected: HashSet<i32> = by_recency
        .iter()
        .take(PROTECTED_RECENT)
        .map(|(id, _used, _exited)| *id)
        .collect();
    let mut oldest_first = meta.to_vec();
    oldest_first.sort_by_key(|(_id, used, _exited)| *used);
    let unprotected = || {
        oldest_first
            .iter()
            .filter(|(id, _used, _exited)| !protected.contains(id))
    };
    unprotected()
        .find(|(_id, _used, exited)| *exited)
        .or_else(|| unprotected().next())
        .map(|(id, _used, _exited)| *id)
}

/// Context shared by one process's streaming and exit-watching tasks.
#[derive(Clone)]
struct ExitReporter {
    process: Arc<ExecProcess>,
    sink: Arc<dyn ExecEventSink>,
    tool_call_id: ToolCallId,
    turn_id: TurnId,
    process_id: i32,
    started: Instant,
    phase: watch::Receiver<Phase>,
}

impl ExitReporter {
    /// Publishes the transcript tail whenever it changes, at most every 100 ms,
    /// until the process finishes or the first call's result takes the exit.
    async fn stream(self) {
        let mut phase = self.phase.clone();
        let mut published = 0;
        loop {
            // That result renders the final output; a snapshot would replace it.
            if *phase.borrow() == Phase::ReturnedExit {
                return;
            }
            let finished = self.process.state().finished();
            let total = self.transcript_bytes();
            if total != published {
                published = total;
                self.sink
                    .output(
                        ExecOutputUpdate::builder()
                            .tool_call_id(self.tool_call_id.clone())
                            .turn_id(self.turn_id.clone())
                            .process_id(self.process_id)
                            .output(self.tail().content)
                            .build(),
                    )
                    .await;
            }
            if finished {
                return;
            }
            tokio::select! {
                () = tokio::time::sleep(STREAM_INTERVAL) => {}
                // A closed channel disables this branch, leaving the sleep.
                Ok(()) = phase.changed() => {}
            }
        }
    }

    /// Reports the exit once, unless the first call already returned it.
    async fn watch(self, reported: watch::Sender<bool>) {
        let state = self.process.wait_finished().await;
        let mut phase = self.phase.clone();
        let phase = phase
            .wait_for(|phase| *phase != Phase::Initial)
            .await
            .map_or(Phase::Background, |phase| *phase);
        if phase == Phase::Background {
            let exit = self.exit(&state);
            tracing::info!(
                "exec process {} exited with code {:?} signal {:?} after {} ms",
                self.process_id,
                exit.exit_code,
                exit.signal,
                exit.wall_time_ms
            );
            self.sink.exited(exit).await;
        }
        reported.send_replace(true);
    }

    /// Bytes the process printed so far.
    fn transcript_bytes(&self) -> usize {
        self.process.buffers().transcript.total_bytes()
    }

    /// Visible tail of the transcript, bounded like other tool output. Only
    /// the newest bytes are decoded; one past the limit marks a cut tail.
    fn tail(&self) -> TruncationDetails {
        let newest = self
            .process
            .buffers()
            .transcript
            .last_text(DEFAULT_MAX_BYTES + 1);
        truncate_tail(&newest, DEFAULT_MAX_LINES, DEFAULT_MAX_BYTES)
    }

    /// Builds the exit report from the final state and transcript.
    fn exit(&self, state: &ProcessState) -> ExecExit {
        let tail = self.tail();
        // Only a cut tail needs the whole transcript kept on disk.
        let full_output_path = if tail.truncated {
            let mut buffers = self.process.buffers();
            // Bytes memory dropped survive only in the spilled transcript.
            buffers
                .spilled_transcript()
                .or_else(|| persist_full_output(&buffers.transcript.text()))
                .map(|path| path.to_string_lossy().into_owned())
        } else {
            None
        };
        ExecExit::builder()
            .tool_call_id(self.tool_call_id.clone())
            .turn_id(self.turn_id.clone())
            .process_id(self.process_id)
            .exit_code(state.exit_code)
            .signal(state.signal.clone())
            .output(tail.content)
            .full_output_path(full_output_path)
            .wall_time_ms(
                u64::try_from(self.started.elapsed().as_millis())
                    .unwrap_or(u64::MAX),
            )
            .build()
    }
}

/// The process table of one Session.
pub struct ExecProcessManager {
    settings: Arc<ExecSettings>,
    store: Mutex<ProcessStore>,
    snapshot: SessionSnapshot,
}

impl ExecProcessManager {
    /// Creates an empty table.
    #[must_use]
    pub fn new(settings: Arc<ExecSettings>) -> Self {
        Self {
            snapshot: SessionSnapshot::new(Arc::clone(&settings)),
            settings,
            store: Mutex::new(ProcessStore::default()),
        }
    }

    /// Settings this manager runs with.
    #[must_use]
    pub fn settings(&self) -> &ExecSettings {
        &self.settings
    }

    /// Starts a command and waits until it exits or its yield time passes; a
    /// command still running stays in the table and its session ID is returned.
    pub async fn exec_command(
        &self,
        start: ExecStart,
    ) -> Result<ExecResponse, ExecError> {
        if start.cancellation.is_cancelled() {
            return Err(ExecError::Cancelled);
        }
        let cwd = match &start.args.workdir {
            Some(dir) if dir.is_absolute() => dir.clone(),
            Some(dir) => start.cwd.join(dir),
            None => start.cwd.clone(),
        };
        if !tokio::fs::try_exists(&cwd).await.unwrap_or(false) {
            tracing::warn!(
                "refused exec_command for missing workdir {}",
                cwd.display()
            );
            return Err(ExecError::InvalidWorkdir(cwd));
        }
        let process_id = self.allocate().await;
        let spec = self.spawn_spec(&start, cwd.clone());
        let process = match ExecProcess::spawn(spec) {
            Ok(process) => Arc::new(process),
            Err(error) => {
                self.store.lock().await.reserved.remove(&process_id);
                tracing::warn!("failed to start exec process: {}", error);
                return Err(error);
            }
        };
        // The command itself is never logged: it may carry credentials.
        tracing::info!(
            "started exec process {} (tty {}) for call {}",
            process_id,
            start.args.tty,
            start.tool_call_id
        );
        let started = Instant::now();
        let (phase, phase_receiver) = watch::channel(Phase::Initial);
        let initial = InitialCall { phase };
        let (reported, reported_receiver) = watch::channel(false);
        let reporter = ExitReporter {
            process: Arc::clone(&process),
            sink: Arc::clone(&start.sink),
            tool_call_id: start.tool_call_id.clone(),
            turn_id: start.turn_id.clone(),
            process_id,
            started,
            phase: phase_receiver.clone(),
        };
        let stream = tokio::spawn(reporter.clone().stream());
        tokio::spawn(reporter.watch(reported));
        // Stored before waiting, so a cancelled wait leaves it pollable.
        self.insert(
            process_id,
            ProcessEntry {
                process: Arc::clone(&process),
                command: start.args.cmd.clone(),
                cwd,
                tool_call_id: start.tool_call_id.clone(),
                last_used: started,
                phase: phase_receiver,
                reported: reported_receiver,
            },
        )
        .await;
        let deadline = tokio::time::Instant::now() + start.args.yield_time();
        let state = process.settle(deadline, &start.cancellation).await?;
        let output = process.buffers().take_pending();
        let finished = state.finished();
        if finished {
            initial.finish(Phase::ReturnedExit);
            // A snapshot already being published lands before the result does.
            let _streamed = stream.await;
            self.remove(process_id, &process).await;
        } else {
            initial.finish(Phase::Background);
            tracing::info!(
                "moved exec process {} to background after {} ms",
                process_id,
                started.elapsed().as_millis()
            );
        }
        Ok(Self::respond(
            output,
            &state,
            (!finished).then_some(process_id),
            start.args.max_output_tokens(),
            start.tool_call_id,
            started,
        ))
    }

    /// Polls a session, or writes to it first; returns output since the last
    /// call and the exit once the process finished.
    pub async fn write_stdin(
        &self,
        write: ExecWrite,
    ) -> Result<ExecResponse, ExecError> {
        let process_id = write.args.session_id;
        let process = self.lookup(process_id).await?;
        let lock = process.interaction_lock();
        // The holder may wait out its whole yield time, so waiting is
        // cancellable; cancellation also wins over a free lock.
        let _interaction = tokio::select! {
            biased;
            () = write.cancellation.cancelled() => {
                tracing::info!(
                    "cancelled write_stdin waiting for exec process {}",
                    process_id
                );
                return Err(ExecError::Cancelled);
            }
            guard = lock.lock() => guard,
        };
        // Revalidate under the lock: a concurrent call may have collected it.
        let tool_call_id = self.touch(process_id, &process).await?;
        // Checked after the last await, so a cancelled call never sends input.
        if write.cancellation.is_cancelled() {
            tracing::info!(
                "cancelled write_stdin before sending to exec process {}",
                process_id
            );
            return Err(ExecError::Cancelled);
        }
        let started = Instant::now();
        let chars = &write.args.chars;
        if !chars.is_empty() {
            let written = if process.is_tty() {
                process.write(chars.as_bytes())
            } else if chars == "\u{3}" {
                process.interrupt()
            } else {
                tracing::warn!(
                    "refused input for non-tty exec process {}",
                    process_id
                );
                return Err(ExecError::StdinClosed);
            };
            // A process that already exited still has output to collect.
            if let Err(error) = written
                && !process.state().exited
            {
                tracing::warn!(
                    "failed to write to exec process {}: {}",
                    process_id,
                    error
                );
                return Err(error);
            }
        }
        let max_background = Duration::from_millis(
            self.settings.config.background_terminal_max_timeout_ms,
        );
        let deadline =
            tokio::time::Instant::now() + write.args.yield_time(max_background);
        let state = process.settle(deadline, &write.cancellation).await?;
        let output = process.buffers().take_pending();
        let finished = state.finished();
        if finished {
            self.remove(process_id, &process).await;
        }
        Ok(Self::respond(
            output,
            &state,
            (!finished).then_some(process_id),
            write.args.max_output_tokens(),
            tool_call_id,
            started,
        ))
    }

    /// Sessions the model can still use, excluding processes whose first
    /// `exec_command` has not returned yet.
    pub async fn sessions(&self) -> super::CommandSessions {
        let store = self.store.lock().await;
        store
            .processes
            .iter()
            .filter(|(_id, entry)| *entry.phase.borrow() != Phase::Initial)
            .map(|(id, entry)| {
                (
                    *id,
                    super::CommandSessionState {
                        exited: entry.process.state().exited,
                        in_use: entry
                            .process
                            .interaction_lock()
                            .try_lock()
                            .is_err(),
                    },
                )
            })
            .collect()
    }

    /// Running processes, ordered by session ID.
    pub async fn list(&self) -> Vec<ExecProcessInfo> {
        let store = self.store.lock().await;
        let mut processes: Vec<ExecProcessInfo> = store
            .processes
            .iter()
            .filter(|(_id, entry)| !entry.process.state().exited)
            .map(|(id, entry)| {
                ExecProcessInfo::builder()
                    .process_id(*id)
                    .command(entry.command.clone())
                    .cwd(entry.cwd.clone())
                    .tool_call_id(entry.tool_call_id.clone())
                    .build()
            })
            .collect();
        processes.sort_by_key(|process| process.process_id);
        processes
    }

    /// Kills one session and forgets it; its exit is still reported.
    pub async fn terminate(&self, process_id: i32) -> bool {
        let removed = {
            let mut store = self.store.lock().await;
            store.reserved.remove(&process_id);
            store.processes.remove(&process_id)
        };
        let Some(entry) = removed else {
            return false;
        };
        tracing::info!("terminated exec process {}", process_id);
        entry.process.terminate();
        true
    }

    /// Kills every session, waits up to two seconds for their exit reports,
    /// and returns how many were still running.
    pub async fn terminate_all(&self) -> usize {
        let entries: Vec<(i32, ProcessEntry)> = {
            let mut store = self.store.lock().await;
            let entries: Vec<_> = store.processes.drain().collect();
            for (id, _entry) in &entries {
                store.reserved.remove(id);
            }
            entries
        };
        let running = entries
            .iter()
            .filter(|(_id, entry)| !entry.process.state().exited)
            .count();
        for (_id, entry) in &entries {
            entry.process.terminate();
        }
        let waited = tokio::time::timeout(REPORT_WAIT, async {
            for (_id, entry) in &entries {
                let mut reported = entry.reported.clone();
                let _settled = reported.wait_for(|reported| *reported).await;
            }
        })
        .await;
        if waited.is_err() {
            tracing::warn!(
                "timed out waiting for exit reports of {} exec processes",
                entries.len()
            );
        }
        tracing::info!("terminated {} running exec processes", running);
        running
    }

    /// Hands out an unused session ID.
    async fn allocate(&self) -> i32 {
        let mut store = self.store.lock().await;
        loop {
            let id = fastrand::i32(1_000..100_000);
            if store.reserved.insert(id) {
                return id;
            }
        }
    }

    /// Builds the child command line and environment for one call.
    fn spawn_spec(&self, start: &ExecStart, cwd: PathBuf) -> SpawnSpec {
        // Non-UTF-8 variables cannot be represented and are not passed on.
        let parent = std::env::vars_os().filter_map(|(name, value)| {
            Some((name.into_string().ok()?, value.into_string().ok()?))
        });
        let env = ExecEnvironment::new(&self.settings.config.env).build(
            parent,
            &start.session_id,
            &start.turn_id,
        );
        let shell = start.args.shell.clone().map_or_else(
            || UserShell::detect(std::env::var("SHELL").ok().as_deref()),
            UserShell::from,
        );
        let snapshot =
            self.snapshot
                .for_shell(&shell, &env, &cwd, &start.session_id);
        let exports = ExecEnvironment::new(&self.settings.config.env)
            .overrides(&start.session_id, &start.turn_id);
        SpawnSpec::builder()
            .argv(shell.argv(
                &start.args.cmd,
                start.args.login,
                snapshot.as_deref(),
                &exports,
            ))
            .cwd(cwd)
            .env(env)
            .tty(start.args.tty)
            .build()
    }

    /// Stops a capture still running, then deletes this Session's snapshot file.
    pub async fn remove_snapshot(&self) {
        self.snapshot.remove().await;
    }

    /// Stores a new process, evicting one when the table is full.
    async fn insert(&self, process_id: i32, entry: ProcessEntry) {
        let evicted = {
            let mut store = self.store.lock().await;
            let evicted = if store.processes.len() >= MAX_PROCESSES {
                store.evict()
            } else {
                None
            };
            store.processes.insert(process_id, entry);
            evicted
        };
        // Terminated outside the lock; its watcher reports the exit.
        if let Some((id, entry)) = evicted {
            tracing::info!("evicted exec process {} from a full table", id);
            entry.process.terminate();
        }
    }

    /// Returns the live process behind a session ID.
    async fn lookup(
        &self,
        process_id: i32,
    ) -> Result<Arc<ExecProcess>, ExecError> {
        let store = self.store.lock().await;
        store
            .processes
            .get(&process_id)
            .map(|entry| Arc::clone(&entry.process))
            .ok_or_else(|| {
                tracing::warn!("unknown exec process {}", process_id);
                ExecError::UnknownProcessId(process_id)
            })
    }

    /// Confirms the id still names `process` and marks it used.
    async fn touch(
        &self,
        process_id: i32,
        process: &Arc<ExecProcess>,
    ) -> Result<ToolCallId, ExecError> {
        let mut store = self.store.lock().await;
        match store.processes.get_mut(&process_id) {
            Some(entry) if Arc::ptr_eq(&entry.process, process) => {
                entry.last_used = Instant::now();
                Ok(entry.tool_call_id.clone())
            }
            _ => {
                tracing::warn!(
                    "exec process {} was already collected",
                    process_id
                );
                Err(ExecError::UnknownProcessId(process_id))
            }
        }
    }

    /// Forgets a collected process, unless the id was reused meanwhile.
    async fn remove(&self, process_id: i32, process: &Arc<ExecProcess>) {
        let mut store = self.store.lock().await;
        if store
            .processes
            .get(&process_id)
            .is_some_and(|entry| Arc::ptr_eq(&entry.process, process))
        {
            store.processes.remove(&process_id);
            store.reserved.remove(&process_id);
        }
    }

    /// Formats one call's result, keeping full output on disk when truncated;
    /// `spilled` names the transcript file when the output lost bytes.
    fn respond(
        (output, spilled): (Captured, Option<PathBuf>),
        state: &ProcessState,
        process_id: Option<i32>,
        max_output_tokens: usize,
        terminal_call_id: ToolCallId,
        started: Instant,
    ) -> ExecResponse {
        let finished = process_id.is_none();
        let mut response = ExecResponse::builder()
            .chunk_id(format!("{:06x}", fastrand::u32(..0x0100_0000)))
            .wall_time(started.elapsed())
            .process_id(process_id)
            .exit_code(state.exit_code.filter(|_code| finished))
            .signal(state.signal.clone().filter(|_signal| finished))
            .output(output)
            .max_output_tokens(max_output_tokens)
            .terminal_call_id(terminal_call_id)
            .build();
        if response.is_truncated() {
            response.full_output_path = spilled
                .or_else(|| persist_full_output(&response.output.text()));
        }
        response
    }
}

impl Drop for ExecProcessManager {
    /// Kills every retained process; reporter tasks would otherwise keep them alive.
    fn drop(&mut self) {
        if let Ok(store) = self.store.try_lock() {
            for entry in store.processes.values() {
                entry.process.terminate();
            }
        }
    }
}
