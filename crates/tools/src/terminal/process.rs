use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use protocol::{TerminalId, TerminalSnapshot, TerminalStatus, TimestampMs};
use tokio::sync::watch;

use super::backend::{TerminalBackend, TerminalExit};
use super::buffer::{TerminalOutputBuffer, TerminalOutputSlice};
use super::{
    TERMINAL_OUTPUT_CAPACITY, TerminalError, TerminalInteractionState,
};

/// Internal terminal lifecycle with retained failure diagnostics.
#[derive(Debug, Clone)]
enum ProcessLifecycle {
    Running,
    Terminating,
    Exited(Option<i32>),
    Failed(String),
}

/// Mutable process state protected by one short synchronous critical section.
struct ProcessState {
    lifecycle: ProcessLifecycle,
    output: TerminalOutputBuffer,
    last_activity_at: TimestampMs,
}

/// Immutable identity and launch metadata for one registered terminal process.
#[derive(typed_builder::TypedBuilder)]
pub(crate) struct TerminalProcessMetadata {
    id: TerminalId,
    command: String,
    cwd: PathBuf,
    tty: bool,
    started_at: TimestampMs,
}

/// One Session-owned process, its bounded output, and interaction serialization.
#[derive(typed_builder::TypedBuilder)]
pub(crate) struct TerminalProcess {
    id: TerminalId,
    command: String,
    cwd: PathBuf,
    tty: bool,
    started_at: TimestampMs,
    state: Mutex<ProcessState>,
    pub(crate) interaction: tokio::sync::Mutex<()>,
    control: Arc<dyn TerminalBackend>,
    changes: watch::Sender<u64>,
    revisions: watch::Sender<u64>,
}

impl TerminalProcess {
    /// Builds one registered process around an already-spawned backend control handle.
    pub(crate) fn new(
        metadata: TerminalProcessMetadata,
        control: Arc<dyn TerminalBackend>,
        revisions: watch::Sender<u64>,
    ) -> Self {
        let (changes, _receiver) = watch::channel(0_u64);
        Self::builder()
            .id(metadata.id)
            .command(metadata.command)
            .cwd(metadata.cwd)
            .tty(metadata.tty)
            .started_at(metadata.started_at)
            .state(Self::initial_state(metadata.started_at))
            .interaction(tokio::sync::Mutex::new(()))
            .control(control)
            .changes(changes)
            .revisions(revisions)
            .build()
    }

    /// Creates the mutable state used when a backend first becomes visible.
    fn initial_state(started_at: TimestampMs) -> Mutex<ProcessState> {
        Mutex::new(ProcessState {
            lifecycle: ProcessLifecycle::Running,
            output: TerminalOutputBuffer::new(TERMINAL_OUTPUT_CAPACITY),
            last_activity_at: started_at,
        })
    }

    /// Returns this process's Session-local terminal identifier.
    pub(crate) const fn id(&self) -> TerminalId {
        self.id
    }

    /// Returns whether ordinary interactive input is supported.
    pub(crate) const fn tty(&self) -> bool {
        self.tty
    }

    /// Clones the backend control capability without cloning process resources.
    pub(crate) fn control(&self) -> Arc<dyn TerminalBackend> {
        Arc::clone(&self.control)
    }

    /// Subscribes to output and lifecycle version changes without a lost-wakeup race.
    pub(crate) fn subscribe_changes(&self) -> watch::Receiver<u64> {
        self.changes.subscribe()
    }

    /// Returns whether the backend has reached a terminal lifecycle state.
    pub(crate) fn is_complete(&self) -> Result<bool, TerminalError> {
        let state = self
            .state
            .lock()
            .map_err(|_error| TerminalError::StatePoisoned)?;
        Ok(matches!(
            state.lifecycle,
            ProcessLifecycle::Exited(_) | ProcessLifecycle::Failed(_)
        ))
    }

    /// Publishes termination intent before the backend control call begins.
    pub(crate) fn begin_termination(&self) -> Result<bool, TerminalError> {
        let changed = {
            let mut state = self
                .state
                .lock()
                .map_err(|_error| TerminalError::StatePoisoned)?;
            if matches!(state.lifecycle, ProcessLifecycle::Running) {
                state.lifecycle = ProcessLifecycle::Terminating;
                state.last_activity_at = TimestampMs::now();
                true
            } else {
                false
            }
        };
        if changed {
            self.bump_host_revision();
            self.bump_change();
        }
        Ok(changed)
    }

    /// Restores interaction after a backend rejects termination.
    pub(crate) fn restore_after_termination_failure(
        &self,
    ) -> Result<(), TerminalError> {
        let changed = {
            let mut state = self
                .state
                .lock()
                .map_err(|_error| TerminalError::StatePoisoned)?;
            if matches!(state.lifecycle, ProcessLifecycle::Terminating) {
                state.lifecycle = ProcessLifecycle::Running;
                state.last_activity_at = TimestampMs::now();
                true
            } else {
                false
            }
        };
        if changed {
            self.bump_host_revision();
            self.bump_change();
        }
        Ok(())
    }

    /// Rejects model interaction once process termination has started.
    pub(crate) fn ensure_interactable(&self) -> Result<(), TerminalError> {
        let state = self
            .state
            .lock()
            .map_err(|_error| TerminalError::StatePoisoned)?;
        if matches!(state.lifecycle, ProcessLifecycle::Terminating) {
            Err(TerminalError::UnknownTerminal {
                terminal_id: self.id,
            })
        } else {
            Ok(())
        }
    }

    /// Records input activity and wakes observers waiting for a state boundary.
    pub(crate) fn touch(&self) -> Result<(), TerminalError> {
        let mut state = self
            .state
            .lock()
            .map_err(|_error| TerminalError::StatePoisoned)?;
        state.last_activity_at = TimestampMs::now();
        drop(state);
        // Explicit Agent input is a bounded interaction boundary, so it can
        // invalidate the host snapshot without forwarding raw output traffic.
        self.bump_host_revision();
        self.bump_change();
        Ok(())
    }

    /// Appends one backend output chunk without forwarding unbounded host traffic.
    pub(crate) fn append_output(
        &self,
        chunk: &[u8],
    ) -> Result<(), TerminalError> {
        {
            let mut state = self
                .state
                .lock()
                .map_err(|_error| TerminalError::StatePoisoned)?;
            state.last_activity_at = TimestampMs::now();
            state.output.append(chunk);
        }
        self.bump_change();
        Ok(())
    }

    /// Applies the final backend result and invalidates the host snapshot once.
    pub(crate) fn finish(
        &self,
        exit: TerminalExit,
    ) -> Result<(), TerminalError> {
        {
            let mut state = self
                .state
                .lock()
                .map_err(|_error| TerminalError::StatePoisoned)?;
            state.last_activity_at = TimestampMs::now();
            if let Some(message) = exit.failure {
                state.lifecycle = ProcessLifecycle::Failed(message);
            } else {
                state.lifecycle = ProcessLifecycle::Exited(exit.exit_code);
            }
        }
        self.bump_host_revision();
        self.bump_change();
        Ok(())
    }

    /// Drains one non-repeating Agent output snapshot under the process lock.
    pub(crate) fn drain_output(
        &self,
        max_output_tokens: usize,
    ) -> Result<TerminalOutputSlice, TerminalError> {
        let mut state = self
            .state
            .lock()
            .map_err(|_error| TerminalError::StatePoisoned)?;
        Ok(state.output.drain_agent(max_output_tokens))
    }

    /// Returns unread Agent output for UI streaming without consuming it.
    pub(crate) fn snapshot_output(
        &self,
        max_output_tokens: usize,
    ) -> Result<TerminalOutputSlice, TerminalError> {
        let state = self
            .state
            .lock()
            .map_err(|_error| TerminalError::StatePoisoned)?;
        Ok(state.output.snapshot_agent(max_output_tokens))
    }

    /// Projects the internal lifecycle into one model-facing interaction state.
    pub(crate) fn interaction_state(
        &self,
    ) -> Result<TerminalInteractionState, TerminalError> {
        let state = self
            .state
            .lock()
            .map_err(|_error| TerminalError::StatePoisoned)?;
        Ok(match &state.lifecycle {
            ProcessLifecycle::Running => TerminalInteractionState::Running {
                session_id: self.id,
            },
            ProcessLifecycle::Terminating => {
                return Err(TerminalError::UnknownTerminal {
                    terminal_id: self.id,
                });
            }
            ProcessLifecycle::Exited(exit_code) => {
                TerminalInteractionState::Exited {
                    exit_code: *exit_code,
                }
            }
            ProcessLifecycle::Failed(message) => {
                TerminalInteractionState::Failed {
                    message: message.clone(),
                }
            }
        })
    }

    /// Creates a non-consuming host snapshot of current process state.
    pub(crate) fn snapshot(&self) -> Result<TerminalSnapshot, TerminalError> {
        let state = self
            .state
            .lock()
            .map_err(|_error| TerminalError::StatePoisoned)?;
        let (status, exit_code) = match &state.lifecycle {
            ProcessLifecycle::Running | ProcessLifecycle::Terminating => {
                (TerminalStatus::Running, None)
            }
            ProcessLifecycle::Exited(exit_code) => {
                (TerminalStatus::Exited, *exit_code)
            }
            ProcessLifecycle::Failed(message) => {
                let _diagnostic_retained_for_debugging = message;
                (TerminalStatus::Failed, None)
            }
        };
        Ok(TerminalSnapshot::builder()
            .terminal_id(self.id)
            .command(self.command.clone())
            .cwd(self.cwd.clone())
            .tty(self.tty)
            .status(status)
            .exit_code(exit_code)
            .started_at(self.started_at)
            .last_activity_at(state.last_activity_at)
            .build())
    }

    /// Advances the process-local change version and wakes all current waiters.
    fn bump_change(&self) {
        self.changes.send_modify(|version| {
            *version = version.saturating_add(1);
        });
    }

    /// Advances the Session-visible snapshot revision at a bounded state boundary.
    fn bump_host_revision(&self) {
        self.revisions
            .send_modify(|revision| *revision = revision.saturating_add(1));
    }
}
