use std::collections::{BTreeMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use protocol::{
    SessionId, TerminalId, TerminalListResult, TerminalRemovalReason,
    TimestampMs,
};
use tokio::sync::watch;

use super::backend::{TerminalExit, spawn_backend};
use super::buffer::TerminalOutputSlice;
use super::process::{TerminalProcess, TerminalProcessMetadata};
use super::{
    TerminalError, TerminalInteractionOutput, TerminalInteractionRequest,
    TerminalInteractionState, TerminalOutputSink, TerminalService,
    TerminalSpawnRequest,
};

const MAX_TERMINALS: usize = 64;
const PROTECTED_RECENT_TERMINALS: usize = 8;

/// Synchronously projected manager table and least-recently-used ordering.
struct ManagerState {
    entries: BTreeMap<TerminalId, Arc<TerminalProcess>>,
    lru: VecDeque<TerminalId>,
    accepting_spawns: bool,
}

impl Default for ManagerState {
    /// Creates an empty live manager state that accepts process creation.
    fn default() -> Self {
        Self {
            entries: BTreeMap::new(),
            lru: VecDeque::new(),
            accepting_spawns: true,
        }
    }
}

impl ManagerState {
    /// Moves one retained identifier to the newest end of the LRU ordering.
    fn touch(&mut self, terminal_id: TerminalId) {
        self.lru.retain(|candidate| *candidate != terminal_id);
        self.lru.push_back(terminal_id);
    }

    /// Removes one retained process and its LRU bookkeeping atomically.
    fn remove(
        &mut self,
        terminal_id: TerminalId,
    ) -> Option<Arc<TerminalProcess>> {
        self.lru.retain(|candidate| *candidate != terminal_id);
        self.entries.remove(&terminal_id)
    }

    /// Allocates one unused random terminal identifier with deterministic fallback.
    fn allocate_id(&self) -> Result<TerminalId, TerminalError> {
        for _attempt in 0..256 {
            let candidate = fastrand::u32(TerminalId::MIN..=TerminalId::MAX);
            let terminal_id =
                TerminalId::try_from(candidate).map_err(|error| {
                    TerminalError::Backend {
                        message: error.to_string(),
                    }
                })?;
            if !self.entries.contains_key(&terminal_id) {
                return Ok(terminal_id);
            }
        }
        for candidate in TerminalId::MIN..=TerminalId::MAX {
            let terminal_id =
                TerminalId::try_from(candidate).map_err(|error| {
                    TerminalError::Backend {
                        message: error.to_string(),
                    }
                })?;
            if !self.entries.contains_key(&terminal_id) {
                return Ok(terminal_id);
            }
        }
        Err(TerminalError::Backend {
            message: "terminal identifier space was exhausted".to_string(),
        })
    }

    /// Selects completed work first, then the oldest running unprotected entry.
    fn capacity_victim(&self) -> Result<Option<TerminalId>, TerminalError> {
        for terminal_id in &self.lru {
            if let Some(process) = self.entries.get(terminal_id)
                && process.is_complete()?
            {
                return Ok(Some(*terminal_id));
            }
        }
        let protected = self
            .lru
            .iter()
            .rev()
            .take(PROTECTED_RECENT_TERMINALS)
            .copied()
            .collect::<Vec<_>>();
        Ok(self
            .lru
            .iter()
            .find(|terminal_id| !protected.contains(terminal_id))
            .copied())
    }
}

/// Session-scoped owner for native terminal processes and their retained state.
#[derive(typed_builder::TypedBuilder)]
pub struct TerminalManager {
    session_id: SessionId,
    state: Mutex<ManagerState>,
    operations: tokio::sync::Mutex<()>,
    revisions: watch::Sender<u64>,
}

impl TerminalManager {
    /// Creates an empty native process manager for one Session.
    #[must_use]
    pub fn new(session_id: SessionId) -> Self {
        let (revisions, _receiver) = watch::channel(0_u64);
        Self::builder()
            .session_id(session_id)
            .state(Mutex::new(ManagerState::default()))
            .operations(tokio::sync::Mutex::new(()))
            .revisions(revisions)
            .build()
    }

    /// Returns one non-consuming snapshot of every retained terminal.
    pub fn list(&self) -> Result<TerminalListResult, TerminalError> {
        loop {
            let revision = *self.revisions.borrow();
            let terminals = {
                let state = self
                    .state
                    .lock()
                    .map_err(|_error| TerminalError::StatePoisoned)?;
                state
                    .entries
                    .values()
                    .map(|process| process.snapshot())
                    .collect::<Result<Vec<_>, _>>()?
            };
            // Retry when a lifecycle transition overlapped snapshot projection;
            // otherwise an old process state could be labeled with a new revision.
            if revision == *self.revisions.borrow() {
                return Ok(TerminalListResult {
                    revision,
                    terminals,
                });
            }
        }
    }

    /// Subscribes one host observer to coalesced snapshot revision changes.
    pub fn subscribe(&self) -> watch::Receiver<u64> {
        self.revisions.subscribe()
    }

    /// Terminates and removes one retained terminal idempotently.
    pub async fn terminate(
        &self,
        terminal_id: TerminalId,
    ) -> Result<bool, TerminalError> {
        // Serialize ownership-changing operations so a successful kill can be
        // matched to the exact retained allocation before it is removed.
        let _operation = self.operations.lock().await;
        let process = {
            let state = self
                .state
                .lock()
                .map_err(|_error| TerminalError::StatePoisoned)?;
            state.entries.get(&terminal_id).cloned()
        };
        let Some(process) = process else {
            return Ok(false);
        };
        if process.begin_termination()?
            && let Err(error) = process.control().terminate().await
        {
            process.restore_after_termination_failure()?;
            return Err(error);
        }
        if self
            .remove_if_current(&process, TerminalRemovalReason::Terminated)?
        {
            tracing::info!(
                "terminated terminal {} for Session {}",
                terminal_id,
                self.session_id
            );
        }
        Ok(true)
    }

    /// Terminates and removes every retained terminal with one lifecycle reason.
    pub async fn terminate_all(
        &self,
        reason: TerminalRemovalReason,
    ) -> Result<usize, TerminalError> {
        // Keep spawn, capacity eviction, and bulk cleanup mutually exclusive
        // while OS process ownership transitions are in progress.
        let _operation = self.operations.lock().await;
        self.terminate_all_locked(reason).await
    }

    /// Permanently closes process creation and cleans every retained terminal.
    pub async fn shutdown(
        &self,
        reason: TerminalRemovalReason,
    ) -> Result<usize, TerminalError> {
        let _operation = self.operations.lock().await;
        {
            let mut state = self
                .state
                .lock()
                .map_err(|_error| TerminalError::StatePoisoned)?;
            state.accepting_spawns = false;
        }
        self.terminate_all_locked(reason).await
    }

    /// Cleans retained processes while the caller holds the operation gate.
    async fn terminate_all_locked(
        &self,
        reason: TerminalRemovalReason,
    ) -> Result<usize, TerminalError> {
        let processes = {
            let state = self
                .state
                .lock()
                .map_err(|_error| TerminalError::StatePoisoned)?;
            state.entries.values().cloned().collect::<Vec<_>>()
        };
        let mut count = 0_usize;
        let mut first_error = None;
        for process in processes {
            let termination = if process.begin_termination()? {
                process.control().terminate().await
            } else {
                Ok(())
            };
            if let Err(error) = termination {
                process.restore_after_termination_failure()?;
                if first_error.is_none() {
                    first_error = Some(error);
                }
                // Retain failed process ownership so a later clean or shutdown
                // can retry instead of orphaning an untracked process tree.
                continue;
            }
            if self.remove_if_current(&process, reason)? {
                count = count.saturating_add(1);
            }
        }
        tracing::info!(
            "cleaned {} terminals for Session {}",
            count,
            self.session_id
        );
        if let Some(error) = first_error {
            Err(error)
        } else {
            Ok(count)
        }
    }

    /// Reclaims one capacity victim before a new process is created.
    async fn reclaim_capacity(&self) -> Result<(), TerminalError> {
        let victim = {
            let state = self
                .state
                .lock()
                .map_err(|_error| TerminalError::StatePoisoned)?;
            if state.entries.len() < MAX_TERMINALS {
                return Ok(());
            }
            let victim_id = state.capacity_victim()?.ok_or_else(|| {
                TerminalError::Backend {
                    message: "terminal capacity has no evictable entry"
                        .to_string(),
                }
            })?;
            state.entries.get(&victim_id).cloned()
        };
        if let Some(process) = victim {
            if process.begin_termination()?
                && let Err(error) = process.control().terminate().await
            {
                process.restore_after_termination_failure()?;
                return Err(error);
            }
            if self
                .remove_if_current(&process, TerminalRemovalReason::Capacity)?
            {
                tracing::info!(
                    "evicted terminal {} for Session {} capacity",
                    process.id(),
                    self.session_id
                );
            }
        }
        Ok(())
    }

    /// Removes an allocation only if the table still owns the same process object.
    fn remove_if_current(
        &self,
        process: &Arc<TerminalProcess>,
        reason: TerminalRemovalReason,
    ) -> Result<bool, TerminalError> {
        let removed = {
            let mut state = self
                .state
                .lock()
                .map_err(|_error| TerminalError::StatePoisoned)?;
            let is_current = state
                .entries
                .get(&process.id())
                .is_some_and(|current| Arc::ptr_eq(current, process));
            is_current.then(|| state.remove(process.id())).flatten()
        };
        if removed.is_some() {
            self.publish_removed(process.id(), reason);
            Ok(true)
        } else {
            Ok(false)
        }
    }

    /// Publishes one removal event after the table no longer exposes the process.
    fn publish_removed(
        &self,
        _terminal_id: TerminalId,
        _reason: TerminalRemovalReason,
    ) {
        self.bump_revision();
    }

    /// Advances the coalesced host snapshot revision after one visible change.
    fn bump_revision(&self) {
        self.revisions
            .send_modify(|revision| *revision = revision.saturating_add(1));
    }

    /// Updates the manager LRU ordering after one successful process interaction.
    fn touch_lru(&self, terminal_id: TerminalId) -> Result<(), TerminalError> {
        let mut state = self
            .state
            .lock()
            .map_err(|_error| TerminalError::StatePoisoned)?;
        if state.entries.contains_key(&terminal_id) {
            state.touch(terminal_id);
        }
        Ok(())
    }

    /// Removes one completed process only when the same allocation is still retained.
    fn reap_if_current(
        &self,
        process: &Arc<TerminalProcess>,
    ) -> Result<(), TerminalError> {
        self.remove_if_current(process, TerminalRemovalReason::Reaped)?;
        Ok(())
    }

    /// Waits for a bounded interaction and drains output exactly once.
    async fn wait_and_collect(
        &self,
        process: Arc<TerminalProcess>,
        yield_duration: Duration,
        max_output_tokens: usize,
        cancellation: tokio_util::sync::CancellationToken,
        output_updates: Arc<dyn TerminalOutputSink>,
    ) -> Result<TerminalInteractionOutput, TerminalError> {
        let started = Instant::now();
        let deadline = tokio::time::Instant::now() + yield_duration;
        let mut changes = process.subscribe_changes();
        let mut last_published = String::new();
        loop {
            process.ensure_interactable()?;
            // Publish cumulative unread output so skipped watch revisions do
            // not lose bytes and the final Agent drain remains authoritative.
            let snapshot = process.snapshot_output(max_output_tokens)?.output;
            if !snapshot.is_empty() && snapshot != last_published {
                output_updates.publish(snapshot.clone());
                last_published = snapshot;
            }
            if process.is_complete()? {
                break;
            }
            tokio::select! {
                () = cancellation.cancelled() => return Err(TerminalError::Cancelled),
                () = tokio::time::sleep_until(deadline) => break,
                changed = changes.changed() => {
                    if changed.is_err() {
                        break;
                    }
                }
            }
        }
        let TerminalOutputSlice {
            output,
            omitted_bytes: _omitted_bytes,
        } = process.drain_output(max_output_tokens)?;
        process.ensure_interactable()?;
        let state = match process.interaction_state()? {
            TerminalInteractionState::Exited { exit_code } => {
                self.reap_if_current(&process)?;
                TerminalInteractionState::Exited { exit_code }
            }
            TerminalInteractionState::Failed { message } => {
                self.reap_if_current(&process)?;
                TerminalInteractionState::Failed { message }
            }
            TerminalInteractionState::Running { session_id } => {
                TerminalInteractionState::Running { session_id }
            }
        };
        self.touch_lru(process.id())?;
        Ok(TerminalInteractionOutput::builder()
            .output(output)
            .state(state)
            .wall_time(started.elapsed())
            .build())
    }

    /// Starts background watchers that continuously drain output and record exit.
    fn watch_backend(
        process: Arc<TerminalProcess>,
        mut output: tokio::sync::mpsc::Receiver<Vec<u8>>,
        exit: tokio::sync::oneshot::Receiver<TerminalExit>,
    ) {
        // The manager table remains the sole strong process owner. Dropping an
        // entry therefore drops backend control even if its watcher is pending.
        let process = Arc::downgrade(&process);
        tokio::spawn(async move {
            while let Some(chunk) = output.recv().await {
                let Some(process) = process.upgrade() else {
                    return;
                };
                if process.append_output(&chunk).is_err() {
                    tracing::warn!("failed to retain terminal output state");
                    break;
                }
            }
            let exit = exit.await.unwrap_or_else(|error| TerminalExit {
                exit_code: None,
                failure: Some(error.to_string()),
            });
            let Some(process) = process.upgrade() else {
                return;
            };
            if process.finish(exit).is_err() {
                tracing::warn!("failed to retain terminal exit state");
            }
        });
    }
}

#[async_trait]
impl TerminalService for TerminalManager {
    /// Starts one Session-owned process and performs its initial bounded wait.
    async fn spawn(
        &self,
        request: TerminalSpawnRequest,
    ) -> Result<TerminalInteractionOutput, TerminalError> {
        if request.session_id != self.session_id {
            return Err(TerminalError::Backend {
                message: "terminal request Session does not match its manager"
                    .to_string(),
            });
        }
        if !request.cwd.is_dir() {
            return Err(TerminalError::InvalidWorkingDirectory {
                path: request.cwd,
            });
        }
        let operation = tokio::select! {
            () = request.cancellation.cancelled() => return Err(TerminalError::Cancelled),
            operation = self.operations.lock() => operation,
        };
        {
            let state = self
                .state
                .lock()
                .map_err(|_error| TerminalError::StatePoisoned)?;
            if !state.accepting_spawns {
                return Err(TerminalError::ShuttingDown);
            }
        }
        self.reclaim_capacity().await?;
        let command = request.cmd.clone();
        let cwd = request.cwd.clone();
        let tty = request.tty;
        let yield_duration = request.yield_duration;
        let max_output_tokens = request.max_output_tokens;
        let cancellation = request.cancellation.clone();
        let output_updates = Arc::clone(&request.output_updates);
        let spawned = spawn_backend(request).await?;
        let terminal_id = {
            let state = self
                .state
                .lock()
                .map_err(|_error| TerminalError::StatePoisoned)?;
            state.allocate_id()?
        };
        let started_at = TimestampMs::now();
        let metadata = TerminalProcessMetadata::builder()
            .id(terminal_id)
            .command(command)
            .cwd(cwd)
            .tty(tty)
            .started_at(started_at)
            .build();
        let process = Arc::new(TerminalProcess::new(
            metadata,
            Arc::clone(&spawned.control),
            self.revisions.clone(),
        ));
        {
            let mut state = self
                .state
                .lock()
                .map_err(|_error| TerminalError::StatePoisoned)?;
            state.entries.insert(terminal_id, Arc::clone(&process));
            state.touch(terminal_id);
        }
        self.bump_revision();
        tracing::info!(
            "started terminal {} for Session {} with tty={} pid={:?}",
            terminal_id,
            self.session_id,
            tty,
            spawned.os_pid
        );
        Self::watch_backend(Arc::clone(&process), spawned.output, spawned.exit);
        drop(operation);
        let result = self
            .wait_and_collect(
                process,
                yield_duration,
                max_output_tokens,
                cancellation,
                output_updates,
            )
            .await?;
        if result.running_id().is_some() {
            tracing::info!(
                "terminal {} remains in background for Session {}",
                terminal_id,
                self.session_id
            );
        }
        Ok(result)
    }

    /// Writes or polls one existing Session-owned process.
    async fn interact(
        &self,
        request: TerminalInteractionRequest,
    ) -> Result<TerminalInteractionOutput, TerminalError> {
        let process = {
            let mut state = self
                .state
                .lock()
                .map_err(|_error| TerminalError::StatePoisoned)?;
            let process =
                state.entries.get(&request.terminal_id).cloned().ok_or(
                    TerminalError::UnknownTerminal {
                        terminal_id: request.terminal_id,
                    },
                )?;
            state.touch(request.terminal_id);
            process
        };
        let interaction = tokio::select! {
            () = request.cancellation.cancelled() => return Err(TerminalError::Cancelled),
            interaction = process.interaction.lock() => interaction,
        };
        // Revalidate after waiting for the per-process gate because host
        // termination may have started after the initial table lookup.
        process.ensure_interactable()?;
        if let Some(chars) = request.chars.as_deref()
            && !chars.is_empty()
        {
            let control = process.control();
            if chars == "\u{3}" {
                control.interrupt().await?;
            } else if process.tty() {
                control.write(chars).await?;
            } else {
                return Err(TerminalError::InputUnsupported {
                    terminal_id: request.terminal_id,
                });
            }
            process.touch()?;
        }
        let result = self
            .wait_and_collect(
                Arc::clone(&process),
                request.yield_duration,
                request.max_output_tokens,
                request.cancellation,
                request.output_updates,
            )
            .await;
        drop(interaction);
        result
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    use protocol::{SessionId, TerminalRemovalReason, TurnId};
    use tokio::sync::Notify;
    use tokio_util::sync::CancellationToken;

    use async_trait::async_trait;

    use super::{
        MAX_TERMINALS, TerminalManager, TerminalProcess,
        TerminalProcessMetadata,
    };
    use crate::terminal::backend::TerminalBackend;
    use crate::{
        TerminalError, TerminalInteractionRequest, TerminalService,
        TerminalSpawnRequest,
    };

    /// Builds one validated Session identifier for manager tests.
    fn session(value: &str) -> SessionId {
        SessionId::try_from(value).expect("session id")
    }

    /// Builds one real pipe process request with a short default wait.
    fn spawn_request(
        session_id: SessionId,
        command: &str,
        cancellation: CancellationToken,
    ) -> TerminalSpawnRequest {
        TerminalSpawnRequest::builder()
            .cmd(command.to_string())
            .cwd(std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")))
            .session_id(session_id)
            .turn_id(TurnId::try_from("manager-turn").expect("turn id"))
            .tty(false)
            .yield_duration(Duration::from_millis(25))
            .max_output_tokens(10_000)
            .cancellation(cancellation)
            .build()
    }

    /// Builds one non-consuming poll request for a retained terminal.
    fn poll_request(
        terminal_id: protocol::TerminalId,
    ) -> TerminalInteractionRequest {
        TerminalInteractionRequest::builder()
            .terminal_id(terminal_id)
            .yield_duration(Duration::from_millis(25))
            .max_output_tokens(10_000)
            .cancellation(CancellationToken::new())
            .build()
    }

    /// Backend that fails its first termination request and accepts the retry.
    struct RetryTerminateBackend {
        terminate_attempts: AtomicUsize,
    }

    /// Backend that exposes the interval while one termination is in progress.
    struct BlockingTerminateBackend {
        entered: Arc<Notify>,
        release: Arc<Notify>,
    }

    #[async_trait]
    impl TerminalBackend for RetryTerminateBackend {
        /// Rejects writes because the ownership test only exercises termination.
        async fn write(&self, _chars: &str) -> Result<(), TerminalError> {
            Err(TerminalError::Backend {
                message: "test backend does not accept input".to_string(),
            })
        }

        /// Rejects interrupts because the ownership test only exercises termination.
        async fn interrupt(&self) -> Result<(), TerminalError> {
            Err(TerminalError::Backend {
                message: "test backend does not accept interrupts".to_string(),
            })
        }

        /// Fails once so the manager must retain ownership for a later retry.
        async fn terminate(&self) -> Result<(), TerminalError> {
            if self.terminate_attempts.fetch_add(1, Ordering::SeqCst) == 0 {
                Err(TerminalError::Backend {
                    message: "injected termination failure".to_string(),
                })
            } else {
                Ok(())
            }
        }
    }

    #[async_trait]
    impl TerminalBackend for BlockingTerminateBackend {
        /// Rejects writes because the race test performs a pure poll.
        async fn write(&self, _chars: &str) -> Result<(), TerminalError> {
            Err(TerminalError::Backend {
                message: "test backend does not accept input".to_string(),
            })
        }

        /// Rejects interrupts because the race test performs a pure poll.
        async fn interrupt(&self) -> Result<(), TerminalError> {
            Err(TerminalError::Backend {
                message: "test backend does not accept interrupts".to_string(),
            })
        }

        /// Pauses termination so concurrent interaction can observe its lifecycle.
        async fn terminate(&self) -> Result<(), TerminalError> {
            self.entered.notify_one();
            self.release.notified().await;
            Ok(())
        }
    }

    /// Inserts one controlled running process without adding test-only production APIs.
    fn insert_retry_process(manager: &TerminalManager) -> protocol::TerminalId {
        insert_retry_process_with_id(manager, 7_319)
    }

    /// Inserts one controlled running process under a selected valid identifier.
    fn insert_retry_process_with_id(
        manager: &TerminalManager,
        value: u32,
    ) -> protocol::TerminalId {
        let terminal_id = protocol::TerminalId::try_from(value)
            .expect("controlled terminal id");
        let metadata = TerminalProcessMetadata::builder()
            .id(terminal_id)
            .command("controlled test process".to_string())
            .cwd(std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")))
            .tty(false)
            .started_at(protocol::TimestampMs::now())
            .build();
        let process = Arc::new(TerminalProcess::new(
            metadata,
            Arc::new(RetryTerminateBackend {
                terminate_attempts: AtomicUsize::new(0),
            }),
            manager.revisions.clone(),
        ));
        let mut state = manager.state.lock().expect("manager state");
        state.entries.insert(terminal_id, process);
        state.touch(terminal_id);
        terminal_id
    }

    /// Waits until a newly spawned process becomes visible to synchronous list callers.
    async fn wait_for_registration(manager: &TerminalManager) {
        tokio::time::timeout(Duration::from_secs(2), async {
            while manager.list().expect("list terminals").terminals.is_empty() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("terminal registration timeout");
    }

    /// Cancelling initial wait leaves an already registered process Session-owned.
    #[tokio::test]
    async fn running_process_is_registered_before_initial_wait_is_cancelled() {
        let session_id = session("cancelled-spawn");
        let manager = Arc::new(TerminalManager::new(session_id.clone()));
        let cancellation = CancellationToken::new();
        let task = tokio::spawn({
            let manager = Arc::clone(&manager);
            let request =
                spawn_request(session_id, "sleep 30", cancellation.clone());
            async move { manager.spawn(request).await }
        });
        wait_for_registration(&manager).await;
        cancellation.cancel();
        assert!(matches!(
            task.await.expect("spawn task"),
            Err(TerminalError::Cancelled)
        ));
        assert_eq!(manager.list().expect("list").terminals.len(), 1);
        manager
            .terminate_all(TerminalRemovalReason::SessionClosed)
            .await
            .expect("cleanup");
    }

    /// A failed backend kill keeps manager ownership so the host can retry cleanup.
    #[tokio::test]
    async fn terminal_manager_retains_process_after_termination_failure() {
        let manager = TerminalManager::new(session("termination-retry"));
        let terminal_id = insert_retry_process(&manager);

        manager
            .terminate(terminal_id)
            .await
            .expect_err("injected termination failure");
        assert_eq!(
            manager.list().expect("list after failure").terminals.len(),
            1
        );
        assert!(
            manager
                .terminate(terminal_id)
                .await
                .expect("retry terminate")
        );
        assert!(
            manager
                .list()
                .expect("list after retry")
                .terminals
                .is_empty()
        );
    }

    /// A process being terminated cannot be rediscovered by a concurrent poll.
    #[tokio::test]
    async fn terminal_manager_rejects_interaction_during_termination() {
        let manager =
            Arc::new(TerminalManager::new(session("termination-race")));
        let entered = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());
        let terminal_id = protocol::TerminalId::try_from(7_320_u32)
            .expect("controlled terminal id");
        let metadata = TerminalProcessMetadata::builder()
            .id(terminal_id)
            .command("controlled race process".to_string())
            .cwd(std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")))
            .tty(false)
            .started_at(protocol::TimestampMs::now())
            .build();
        let process = Arc::new(TerminalProcess::new(
            metadata,
            Arc::new(BlockingTerminateBackend {
                entered: Arc::clone(&entered),
                release: Arc::clone(&release),
            }),
            manager.revisions.clone(),
        ));
        {
            let mut state = manager.state.lock().expect("manager state");
            state.entries.insert(terminal_id, process);
            state.touch(terminal_id);
        }
        let terminate = tokio::spawn({
            let manager = Arc::clone(&manager);
            async move { manager.terminate(terminal_id).await }
        });
        entered.notified().await;

        let interaction = tokio::time::timeout(
            Duration::from_secs(1),
            manager.interact(poll_request(terminal_id)),
        )
        .await
        .expect("interaction must not wait for backend termination");
        assert!(matches!(
            interaction,
            Err(TerminalError::UnknownTerminal { terminal_id: id }) if id == terminal_id
        ));

        release.notify_waiters();
        assert!(
            terminate
                .await
                .expect("join termination")
                .expect("complete termination")
        );
    }

    /// A backend watcher does not become an independent process owner.
    #[tokio::test]
    async fn terminal_backend_watcher_holds_only_weak_process_ownership() {
        let manager = TerminalManager::new(session("watcher-ownership"));
        let terminal_id = protocol::TerminalId::try_from(7_321_u32)
            .expect("controlled terminal id");
        let metadata = TerminalProcessMetadata::builder()
            .id(terminal_id)
            .command("controlled watcher process".to_string())
            .cwd(std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")))
            .tty(false)
            .started_at(protocol::TimestampMs::now())
            .build();
        let process = Arc::new(TerminalProcess::new(
            metadata,
            Arc::new(RetryTerminateBackend {
                terminate_attempts: AtomicUsize::new(0),
            }),
            manager.revisions.clone(),
        ));
        let weak_process = Arc::downgrade(&process);
        let (_output_sender, output_receiver) = tokio::sync::mpsc::channel(1);
        let (_exit_sender, exit_receiver) = tokio::sync::oneshot::channel();

        TerminalManager::watch_backend(
            Arc::clone(&process),
            output_receiver,
            exit_receiver,
        );
        drop(process);
        tokio::task::yield_now().await;

        assert!(
            weak_process.upgrade().is_none(),
            "watcher must not retain process ownership after manager removal"
        );
    }

    /// Bulk cleanup retains a process whose backend did not confirm termination.
    #[tokio::test]
    async fn terminal_manager_retains_bulk_cleanup_failure_for_retry() {
        let manager = TerminalManager::new(session("bulk-termination-retry"));
        insert_retry_process(&manager);

        manager
            .terminate_all(TerminalRemovalReason::Cleaned)
            .await
            .expect_err("injected bulk termination failure");
        assert_eq!(
            manager.list().expect("list after failure").terminals.len(),
            1
        );
        assert_eq!(
            manager
                .terminate_all(TerminalRemovalReason::Cleaned)
                .await
                .expect("retry bulk cleanup"),
            1
        );
    }

    /// Explicit terminal activity invalidates host snapshots without streaming output.
    #[tokio::test]
    async fn terminal_input_activity_advances_the_host_revision() {
        let manager = TerminalManager::new(session("input-activity-revision"));
        let terminal_id = insert_retry_process(&manager);
        let process = manager
            .state
            .lock()
            .expect("manager state")
            .entries
            .get(&terminal_id)
            .cloned()
            .expect("controlled process");
        let mut revisions = manager.subscribe();
        let initial_revision = *revisions.borrow();

        process.touch().expect("record input activity");

        tokio::time::timeout(Duration::from_secs(1), revisions.changed())
            .await
            .expect("host revision notification")
            .expect("manager revision sender");
        assert!(*revisions.borrow_and_update() > initial_revision);
    }

    /// Capacity failure retains the selected victim instead of losing ownership.
    #[tokio::test]
    async fn terminal_manager_retains_capacity_victim_after_kill_failure() {
        let session_id = session("capacity-termination-failure");
        let manager = TerminalManager::new(session_id.clone());
        for value in protocol::TerminalId::MIN
            ..protocol::TerminalId::MIN
                + u32::try_from(MAX_TERMINALS).expect("capacity")
        {
            insert_retry_process_with_id(&manager, value);
        }
        let victim = protocol::TerminalId::try_from(protocol::TerminalId::MIN)
            .expect("oldest terminal id");

        manager
            .spawn(spawn_request(
                session_id,
                "sleep 30",
                CancellationToken::new(),
            ))
            .await
            .expect_err("capacity kill failure");

        let terminals = manager
            .list()
            .expect("list after capacity failure")
            .terminals;
        assert_eq!(terminals.len(), MAX_TERMINALS);
        assert!(
            terminals
                .iter()
                .any(|terminal| terminal.terminal_id == victim)
        );
        manager
            .terminate_all(TerminalRemovalReason::SessionClosed)
            .await
            .expect_err("first retry fails for another controlled backend");
    }

    /// Permanent shutdown wins over queued and future process creation.
    #[tokio::test]
    async fn terminal_manager_shutdown_rejects_queued_and_future_spawns() {
        let session_id = session("shutdown-spawn-gate");
        let manager = Arc::new(TerminalManager::new(session_id.clone()));
        let gate = manager.operations.lock().await;
        let shutdown = tokio::spawn({
            let manager = Arc::clone(&manager);
            async move {
                manager
                    .shutdown(TerminalRemovalReason::KernelShutdown)
                    .await
            }
        });
        tokio::task::yield_now().await;
        let queued_spawn = tokio::spawn({
            let manager = Arc::clone(&manager);
            let request = spawn_request(
                session_id.clone(),
                "sleep 30",
                CancellationToken::new(),
            );
            async move { manager.spawn(request).await }
        });
        drop(gate);

        assert_eq!(
            shutdown.await.expect("shutdown task").expect("shutdown"),
            0
        );
        assert!(matches!(
            queued_spawn.await.expect("queued spawn task"),
            Err(TerminalError::ShuttingDown)
        ));
        assert!(matches!(
            manager
                .spawn(spawn_request(
                    session_id,
                    "sleep 30",
                    CancellationToken::new(),
                ))
                .await,
            Err(TerminalError::ShuttingDown)
        ));
        assert!(manager.list().expect("shutdown list").terminals.is_empty());
    }

    /// Session-local identifiers never authorize access through another manager.
    #[tokio::test]
    async fn managers_reject_each_others_terminal_ids() {
        let first_session = session("first");
        let first = TerminalManager::new(first_session.clone());
        let second = TerminalManager::new(session("second"));
        let running = first
            .spawn(spawn_request(
                first_session,
                "sleep 30",
                CancellationToken::new(),
            ))
            .await
            .expect("spawn first");
        let id = running.running_id().expect("running id");
        assert!(matches!(
            second.interact(poll_request(id)).await,
            Err(TerminalError::UnknownTerminal { .. })
        ));
        first
            .terminate_all(TerminalRemovalReason::SessionClosed)
            .await
            .expect("cleanup");
    }

    /// Agent output advances across interactions and reaps a completed process once.
    #[tokio::test]
    async fn interactions_do_not_replay_output_and_reap_after_exit() {
        let session_id = session("cursor");
        let manager = TerminalManager::new(session_id.clone());
        let first = manager
            .spawn(spawn_request(
                session_id,
                "printf first; sleep 0.15; printf second",
                CancellationToken::new(),
            ))
            .await
            .expect("spawn output process");
        assert!(first.output.contains("first"));
        let id = first.running_id().expect("background terminal");
        let mut poll = poll_request(id);
        poll.yield_duration = Duration::from_secs(1);
        let second = manager.interact(poll).await.expect("collect exit");
        assert!(!second.output.contains("first"));
        assert!(second.output.contains("second"));
        assert!(second.running_id().is_none());
        assert!(
            manager
                .list()
                .expect("list after reap")
                .terminals
                .is_empty()
        );
    }

    /// Pipe processes reject ordinary input while retaining the running entry.
    #[tokio::test]
    async fn pipe_process_rejects_ordinary_interactive_input() {
        let session_id = session("pipe-input");
        let manager = TerminalManager::new(session_id.clone());
        let running = manager
            .spawn(spawn_request(
                session_id,
                "sleep 30",
                CancellationToken::new(),
            ))
            .await
            .expect("spawn pipe process");
        let terminal_id = running.running_id().expect("running terminal");
        let request = TerminalInteractionRequest::builder()
            .terminal_id(terminal_id)
            .chars(Some("hello\n".to_string()))
            .yield_duration(Duration::from_millis(1))
            .max_output_tokens(10_000)
            .cancellation(CancellationToken::new())
            .build();
        assert!(matches!(
            manager.interact(request).await,
            Err(TerminalError::InputUnsupported { .. })
        ));
        assert_eq!(manager.list().expect("list").terminals.len(), 1);
        manager
            .terminate_all(TerminalRemovalReason::SessionClosed)
            .await
            .expect("cleanup");
    }

    /// Capacity pressure evicts the oldest running entry and protects the newest set.
    #[tokio::test]
    async fn capacity_evicts_oldest_running_terminal() {
        let session_id = session("capacity");
        let manager = TerminalManager::new(session_id.clone());
        let mut first_id = None;
        for index in 0..65 {
            let mut request = spawn_request(
                session_id.clone(),
                "sleep 30",
                CancellationToken::new(),
            );
            request.yield_duration = Duration::from_millis(1);
            let terminal_id = manager
                .spawn(request)
                .await
                .expect("spawn capacity process")
                .running_id()
                .expect("retained capacity terminal");
            if index == 0 {
                first_id = Some(terminal_id);
            }
        }
        let terminals = manager.list().expect("capacity list").terminals;
        assert_eq!(terminals.len(), 64);
        assert!(
            !terminals
                .iter()
                .any(|terminal| { Some(terminal.terminal_id) == first_id })
        );
        manager
            .terminate_all(TerminalRemovalReason::SessionClosed)
            .await
            .expect("cleanup");
    }
}
