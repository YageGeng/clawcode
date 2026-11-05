use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, RwLock};

use protocol::{
    AgentEvent, AgentEventPayload, EventMetadata, PendingMessages, QueueId,
    RunId, Sequence, TimestampMs, TurnId,
};
use tokio::sync::{Mutex as AsyncMutex, MutexGuard, Notify};
use tokio_util::sync::CancellationToken;

use crate::runtime::{
    EventSink, KernelError, PendingQueue, Session, WaiterCancellation,
};

/// Serialized lifecycle state for one live session runtime.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::runtime) enum SessionLifecycle {
    /// Startup hooks and resource publication are still in progress.
    Starting,
    /// The session accepts normal agent and maintenance operations.
    Active,
    /// Shutdown hooks are running and no new serialized work may start.
    Closing,
    /// Shutdown completed and the runtime is no longer registered.
    Closed,
}

/// Correlation data that must become visible and disappear as one active-Run value.
#[derive(Clone)]
pub(in crate::runtime) struct ActiveRunContext {
    pub(in crate::runtime) run_id: RunId,
    pub(in crate::runtime) turn_id: TurnId,
    pub(in crate::runtime) sink: Arc<dyn EventSink>,
}

/// Owns serialized operation, cancellation, queue, and event sequencing state.
#[derive(typed_builder::TypedBuilder)]
pub(in crate::runtime) struct SessionExecution {
    #[builder(default)]
    submissions: Mutex<Vec<Arc<super::insertion::SubmissionReceipt>>>,
    #[builder(default)]
    pub(super) admission: Mutex<super::admission::AdmissionState>,
    lifecycle: RwLock<SessionLifecycle>,
    /// True while a shutdown is in progress, so waiters learn when it ends.
    #[builder(default = tokio::sync::watch::channel(false).0)]
    closing: tokio::sync::watch::Sender<bool>,
    gate: AsyncMutex<()>,
    #[builder(default)]
    close_requested: Mutex<CancellationToken>,
    /// Cancelled and replaced by `cancel()` to reject client prompts still waiting for the gate.
    #[builder(default)]
    waiters: Mutex<CancellationToken>,
    #[builder(default)]
    active_run: Mutex<Option<ActiveRunContext>>,
    /// Client prompts, such as MCP elicitations and plan approvals, the Run waits on.
    #[builder(default)]
    user_waits: AtomicUsize,
    idle_notify: Notify,
    pub(super) cancellation: Mutex<CancellationToken>,
    pub(super) queue: Mutex<PendingQueue>,
    sequence: AtomicU64,
}

impl SessionExecution {
    /// Acquires the operation gate only while this execution remains active.
    pub(super) async fn acquire_operation(
        &self,
    ) -> Result<MutexGuard<'_, ()>, KernelError> {
        self.ensure_active()?;
        let closing = self.close_requested.lock()?.clone();
        let guard = tokio::select! {
            biased;
            () = closing.cancelled() => return Err(KernelError::SessionClosing),
            guard = self.gate.lock() => guard,
        };
        // Recheck after waiting because shutdown may have started while the
        // caller was queued behind an operation that already held the gate.
        self.ensure_active()?;
        Ok(guard)
    }

    /// Acquires the prompt gate and installs cancellation without an admission gap.
    pub(super) async fn acquire_operation_for_prompt(
        &self,
        cancellation_parent: Option<&CancellationToken>,
        waiters: WaiterCancellation,
    ) -> Result<(MutexGuard<'_, ()>, CancellationToken), KernelError> {
        self.ensure_active()?;
        let closing = self.close_requested.lock()?.clone();
        // Internal input never observes the session-wide waiter generation.
        let waiting = match &waiters {
            WaiterCancellation::Reject => Some(self.waiters.lock()?.clone()),
            WaiterCancellation::Ignore => None,
            WaiterCancellation::Captured(token) => Some(token.clone()),
        };
        let parent = cancellation_parent.cloned().unwrap_or_default();
        let rejected = async {
            match &waiting {
                Some(token) => token.cancelled().await,
                None => std::future::pending().await,
            }
        };
        let guard = tokio::select! {
            biased;
            () = closing.cancelled() => {
                tracing::info!("rejected waiting prompt because its session is closing");
                return Err(waiters.closing());
            }
            () = parent.cancelled() => {
                tracing::info!("rejected waiting prompt because its parent was cancelled");
                return Err(KernelError::PromptCancelled);
            }
            () = rejected => {
                tracing::info!("rejected waiting prompt because its session was cancelled");
                return Err(KernelError::PromptCancelled);
            }
            guard = self.gate.lock() => guard,
        };
        // Shutdown may have started while this prompt was queued behind the gate.
        self.ensure_active().map_err(|error| match error {
            KernelError::SessionClosing => waiters.closing(),
            other => other,
        })?;
        let mut current = self.cancellation.lock()?;
        // `cancel()` rejects waiters while holding this lock, so a cancel either
        // rejects this admission or reaches the token installed below.
        if waiting
            .as_ref()
            .is_some_and(CancellationToken::is_cancelled)
            || parent.is_cancelled()
        {
            tracing::info!("rejected prompt cancelled during admission");
            return Err(KernelError::PromptCancelled);
        }
        let cancellation = cancellation_parent.map_or_else(
            CancellationToken::new,
            CancellationToken::child_token,
        );
        if closing.is_cancelled() {
            match waiters {
                WaiterCancellation::Reject
                | WaiterCancellation::Captured(_) => {
                    return Err(KernelError::PromptCancelled);
                }
                WaiterCancellation::Ignore => cancellation.cancel(),
            }
        }
        *current = cancellation.clone();
        drop(current);
        Ok((guard, cancellation))
    }

    /// Attempts to acquire the operation gate without queueing direct commands.
    pub(super) fn try_acquire_operation(
        &self,
    ) -> Result<Option<MutexGuard<'_, ()>>, KernelError> {
        self.ensure_active()?;
        if self.close_requested.lock()?.is_cancelled() {
            return Err(KernelError::SessionClosing);
        }
        let Ok(guard) = self.gate.try_lock() else {
            return Ok(None);
        };
        self.ensure_active()?;
        Ok(Some(guard))
    }

    /// Stops admission before cancelling the active run, including a racing token install.
    pub(super) fn request_close(&self) -> Result<(), KernelError> {
        self.close_requested.lock()?.cancel();
        self.cancel()
    }

    /// Acquires shutdown's gate without queueing behind work rejected by the close signal.
    pub(in crate::runtime) async fn acquire_close(
        &self,
    ) -> Result<MutexGuard<'_, ()>, KernelError> {
        self.request_close()?;
        let guard = self.gate.lock().await;
        self.ensure_active()?;
        Ok(guard)
    }

    /// Installs a fresh cancellation token for one serialized operation.
    pub(in crate::runtime) fn install_cancellation(
        &self,
        parent: Option<&CancellationToken>,
    ) -> Result<CancellationToken, KernelError> {
        let cancellation = parent.map_or_else(
            CancellationToken::new,
            CancellationToken::child_token,
        );
        if self.close_requested.lock()?.is_cancelled() {
            cancellation.cancel();
        }
        *self.cancellation.lock()? = cancellation.clone();
        Ok(cancellation)
    }

    /// Returns the current operation cancellation token without retaining its lock.
    pub(in crate::runtime) fn cancellation(
        &self,
    ) -> Result<CancellationToken, KernelError> {
        self.cancellation
            .lock()
            .map(|cancellation| cancellation.clone())
            .map_err(KernelError::from)
    }

    /// Returns a defensive snapshot of all pending messages.
    pub(in crate::runtime) fn pending_snapshot(
        &self,
    ) -> Result<PendingMessages, KernelError> {
        self.queue
            .lock()
            .map(|queue| queue.snapshot())
            .map_err(KernelError::from)
    }

    /// Returns pending identifiers in deterministic scheduling order.
    pub(super) fn pending_ids(&self) -> Result<Vec<QueueId>, KernelError> {
        self.queue
            .lock()
            .map(|queue| queue.ids())
            .map_err(KernelError::from)
    }

    /// Reports whether any steering or follow-up message is pending.
    pub(in crate::runtime) fn has_pending(&self) -> Result<bool, KernelError> {
        self.queue
            .lock()
            .map(|queue| !queue.is_empty())
            .map_err(KernelError::from)
    }

    /// Transfers a consumed receipt to the Run holding the execution gate.
    pub(super) fn accept_receipt(
        &self,
        receipt: Arc<super::insertion::SubmissionReceipt>,
    ) -> Result<(), KernelError> {
        self.submissions.lock()?.push(receipt);
        Ok(())
    }

    /// Publishes one authoritative answer to every input consumed by the current Run.
    pub(super) fn settle_submissions(
        &self,
        answer: Arc<crate::runtime::SubmissionAnswer>,
    ) -> Result<(), KernelError> {
        let submissions = std::mem::take(&mut *self.submissions.lock()?);
        for receipt in submissions {
            receipt.resolve(Arc::clone(&answer));
        }
        Ok(())
    }

    /// Captures the stop generation when a protocol prompt arrives.
    pub(super) fn waiter_token(
        &self,
    ) -> Result<CancellationToken, KernelError> {
        Ok(self.waiters.lock()?.clone())
    }

    /// Cancels the current operation and rejects client prompts waiting for the gate.
    pub(in crate::runtime) fn cancel(&self) -> Result<(), KernelError> {
        let current = self.cancellation.lock()?;
        current.cancel();
        // Waiters are replaced under the admission lock so a cancel cannot fall
        // between a prompt's gate acquisition and its token installation.
        let mut waiters = self.waiters.lock()?;
        waiters.cancel();
        *waiters = CancellationToken::new();
        self.close_queue()
    }

    /// Publishes all active-Run correlation fields under one lock.
    pub(in crate::runtime) fn install_run(
        self: &Arc<Self>,
        context: ActiveRunContext,
    ) -> Result<ActiveRunLease, KernelError> {
        let mut active_run = self.active_run.lock()?;
        if active_run.is_some() {
            return Err(KernelError::Protocol(
                "session already has an active run".to_string(),
            ));
        }
        let run_id = context.run_id.clone();
        *active_run = Some(context);
        // A wait whose future was dropped with an earlier Run never settled.
        self.user_waits.store(0, Ordering::SeqCst);
        Ok(ActiveRunLease {
            execution: Arc::clone(self),
            run_id,
        })
    }

    /// Counts one more client prompt the Run waits on.
    ///
    /// Returns true for the first, which is when the client learns the Run
    /// requires action; MCP elicitations and plan approvals share the count.
    pub(in crate::runtime) fn begin_user_wait(&self) -> bool {
        self.user_waits.fetch_add(1, Ordering::SeqCst) == 0
    }

    /// Counts one client prompt as settled.
    ///
    /// Returns true when no other prompt still waits, which is when the client
    /// learns the Run is running again.
    pub(in crate::runtime) fn end_user_wait(&self) -> bool {
        self.user_waits.try_update(
            Ordering::SeqCst,
            Ordering::SeqCst,
            |waits| waits.checked_sub(1),
        ) == Ok(1)
    }

    /// Returns one owned active-Run snapshot without retaining the state lock.
    pub(in crate::runtime) fn active_run(
        &self,
    ) -> Result<Option<ActiveRunContext>, KernelError> {
        self.active_run
            .lock()
            .map(|active_run| active_run.clone())
            .map_err(KernelError::from)
    }

    /// Sends one event raised inside a tool, such as a client prompt, through
    /// the active Run's sink so it stays ordered with the Run's own events.
    pub(in crate::runtime) async fn emit_to_run(
        &self,
        turn_id: TurnId,
        timestamp_ms: TimestampMs,
        payload: AgentEventPayload,
    ) -> Result<(), KernelError> {
        let active_run = self.active_run()?.ok_or_else(|| {
            KernelError::Protocol("Session has no active Run".to_string())
        })?;
        let sequence = self.next_sequence()?;
        active_run
            .sink
            .emit(AgentEvent {
                metadata: EventMetadata {
                    turn_id,
                    timestamp_ms,
                    sequence,
                },
                payload,
            })
            .await?;
        Ok(())
    }

    /// Advances the diagnostic Turn for the currently installed Run.
    pub(super) fn set_active_turn(
        &self,
        run_id: &RunId,
        turn_id: TurnId,
    ) -> Result<(), KernelError> {
        let mut active_run = self.active_run.lock()?;
        let context = active_run.as_mut().ok_or_else(|| {
            KernelError::Protocol("session has no active run".to_string())
        })?;
        if &context.run_id != run_id {
            return Err(KernelError::Protocol(
                "active run changed while updating its Turn".to_string(),
            ));
        }
        context.turn_id = turn_id;
        Ok(())
    }

    /// Reports whether no Run currently owns this execution component.
    pub(in crate::runtime) fn is_idle(&self) -> Result<bool, KernelError> {
        self.active_run().map(|active_run| active_run.is_none())
    }

    /// Waits until the current active Run lease has been released.
    pub(in crate::runtime) async fn wait_for_idle(
        &self,
    ) -> Result<(), KernelError> {
        loop {
            let notified = self.idle_notify.notified();
            if self.is_idle()? {
                return Ok(());
            }
            notified.await;
        }
    }

    /// Allocates the next positive event sequence for this Session.
    pub(in crate::runtime) fn next_sequence(
        &self,
    ) -> Result<Sequence, KernelError> {
        let raw_sequence = self
            .sequence
            .fetch_add(1, Ordering::Relaxed)
            .checked_add(1)
            .ok_or_else(|| {
                KernelError::Protocol("event sequence overflow".to_string())
            })?;
        Sequence::try_from(raw_sequence)
            .map_err(|error| KernelError::Protocol(error.to_string()))
    }

    /// Transitions an active execution into closing while its gate is held.
    pub(super) fn begin_closing(
        &self,
    ) -> Result<ClosingGuard<'_>, KernelError> {
        let mut lifecycle = self.lifecycle.write()?;
        match *lifecycle {
            SessionLifecycle::Starting => Err(KernelError::SessionStarting),
            SessionLifecycle::Active => {
                *lifecycle = SessionLifecycle::Closing;
                self.closing.send_replace(true);
                Ok(ClosingGuard(&self.closing))
            }
            SessionLifecycle::Closing | SessionLifecycle::Closed => {
                Err(KernelError::SessionClosing)
            }
        }
    }

    /// Restores an active lifecycle when shutdown stops before resource teardown.
    pub(super) fn abort_closing(&self) -> Result<(), KernelError> {
        let mut lifecycle = self.lifecycle.write()?;
        match *lifecycle {
            SessionLifecycle::Closing => {
                *lifecycle = SessionLifecycle::Active;
                *self.close_requested.lock()? = CancellationToken::new();
                Ok(())
            }
            SessionLifecycle::Starting
            | SessionLifecycle::Active
            | SessionLifecycle::Closed => Err(KernelError::Protocol(
                "only a closing Session can abort shutdown".to_string(),
            )),
        }
    }

    /// Marks a closing execution closed before its Session is removed.
    pub(super) fn finish_closing(&self) -> Result<(), KernelError> {
        *self.lifecycle.write()? = SessionLifecycle::Closed;
        Ok(())
    }

    /// Subscribes to shutdown progress: `true` while a close is in progress.
    pub(in crate::runtime) fn closing(
        &self,
    ) -> tokio::sync::watch::Receiver<bool> {
        self.closing.subscribe()
    }

    /// Publishes a fully initialized execution as active after its startup checkpoint.
    pub(super) fn finish_starting(&self) -> Result<(), KernelError> {
        let mut lifecycle = self.lifecycle.write()?;
        match *lifecycle {
            SessionLifecycle::Starting => {
                *lifecycle = SessionLifecycle::Active;
                Ok(())
            }
            SessionLifecycle::Active
            | SessionLifecycle::Closing
            | SessionLifecycle::Closed => Err(KernelError::Protocol(
                "only a starting Session can finish startup".to_string(),
            )),
        }
    }

    /// Whether shutdown finished, so the Store may already belong to a new runtime.
    pub(in crate::runtime) fn is_closed(&self) -> Result<bool, KernelError> {
        Ok(*self.lifecycle.read()? == SessionLifecycle::Closed)
    }

    /// Rejects ordinary work before startup completes or after shutdown starts.
    pub(super) fn ensure_active(&self) -> Result<(), KernelError> {
        let lifecycle = self.lifecycle.read()?;
        match *lifecycle {
            SessionLifecycle::Starting => Err(KernelError::SessionStarting),
            SessionLifecycle::Active => Ok(()),
            SessionLifecycle::Closing | SessionLifecycle::Closed => {
                Err(KernelError::SessionClosing)
            }
        }
    }
}

/// Reports a Session's shutdown as settled when the closing operation ends on
/// any path: completion, an abort back to active, or a dropped close future.
#[must_use = "dropping the guard immediately reports the shutdown as settled"]
pub(in crate::runtime) struct ClosingGuard<'a>(
    &'a tokio::sync::watch::Sender<bool>,
);

impl Drop for ClosingGuard<'_> {
    /// Wakes callers, such as title changes, waiting for this shutdown to end.
    fn drop(&mut self) {
        self.0.send_replace(false);
    }
}

/// Clears active-Run correlation when one serialized Run exits any path.
pub(in crate::runtime) struct ActiveRunLease {
    execution: Arc<SessionExecution>,
    run_id: RunId,
}

impl Drop for ActiveRunLease {
    /// Clears only the active Run identity installed by this lease.
    fn drop(&mut self) {
        let _ = self.execution.close_queue();
        // Fail only receipts left unresolved by an operational error or a dropped Run future.
        if let Err(error) = self.execution.settle_submissions(Arc::new(
            crate::runtime::SubmissionAnswer {
                message_id: None,
                text: String::new(),
                outcome: crate::runtime::AnswerOutcome::Failed(
                    "run ended before settlement".into(),
                ),
            },
        )) {
            tracing::warn!(
                "failed to release submission waiters for run {}: {}",
                self.run_id,
                error
            );
        }
        if let Ok(mut active_run) = self.execution.active_run.lock()
            && active_run
                .as_ref()
                .is_some_and(|context| context.run_id == self.run_id)
        {
            *active_run = None;
        }
        self.execution.idle_notify.notify_waiters();
    }
}

impl Session {
    /// Forces all buffered session mutations to durable storage.
    pub(in crate::runtime) fn sync_store(&self) -> Result<(), KernelError> {
        self.transcript.sync()
    }

    /// Acquires the run gate only while this runtime remains active.
    pub(in crate::runtime) async fn acquire_operation(
        &self,
    ) -> Result<MutexGuard<'_, ()>, KernelError> {
        self.execution.acquire_operation().await
    }

    /// Attempts to acquire the run gate without silently queueing direct commands.
    pub(in crate::runtime) fn try_acquire_operation(
        &self,
    ) -> Result<Option<MutexGuard<'_, ()>>, KernelError> {
        self.execution.try_acquire_operation()
    }

    /// Acquires the run gate for prompt input and installs its cancellation atomically.
    pub(in crate::runtime) async fn acquire_operation_for_prompt(
        &self,
        cancellation_parent: Option<&CancellationToken>,
        waiters: WaiterCancellation,
    ) -> Result<(MutexGuard<'_, ()>, CancellationToken), KernelError> {
        self.execution
            .acquire_operation_for_prompt(cancellation_parent, waiters)
            .await
    }

    /// Transitions an active runtime into closing while holding its run gate.
    pub(in crate::runtime) fn begin_closing(
        &self,
    ) -> Result<ClosingGuard<'_>, KernelError> {
        self.execution.begin_closing()
    }

    /// Restores an active runtime when shutdown stops before resource teardown.
    pub(in crate::runtime) fn abort_closing(&self) -> Result<(), KernelError> {
        self.execution.abort_closing()
    }

    /// Marks a closing runtime closed before its final map removal, sealing
    /// its transcript first so a stale handle cannot write after a Resume.
    pub(in crate::runtime) fn finish_closing(&self) -> Result<(), KernelError> {
        let sealed = self.transcript.seal();
        self.execution.finish_closing()?;
        sealed
    }

    /// Publishes this runtime for ordinary operations after startup is durable.
    pub(in crate::runtime) fn finish_starting(
        &self,
    ) -> Result<(), KernelError> {
        self.execution.finish_starting()
    }

    /// Rejects ordinary kernel work before startup completes or after shutdown starts.
    pub(in crate::runtime) fn ensure_active(&self) -> Result<(), KernelError> {
        self.execution.ensure_active()
    }

    /// Preserves a typed extension-host error for run-gated shutdown reentry.
    pub(in crate::runtime) fn ensure_extension_operation(
        &self,
    ) -> Result<(), extension::ExtensionHostError> {
        match self.ensure_active() {
            Ok(()) => Ok(()),
            Err(KernelError::SessionClosing) => {
                Err(extension::ExtensionHostError::SessionClosing)
            }
            Err(error) => {
                Err(extension::ExtensionHostError::Operation(error.to_string()))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex, RwLock};

    use async_trait::async_trait;
    use protocol::{AgentEvent, RunId, TurnId};
    use tokio::sync::{Mutex as AsyncMutex, Notify};
    use tokio_util::sync::CancellationToken;

    use super::*;
    use crate::{EventSink, SinkError};

    struct TestSink;

    #[async_trait]
    impl EventSink for TestSink {
        /// Accepts test events without retaining them.
        async fn emit(&self, _event: AgentEvent) -> Result<(), SinkError> {
            Ok(())
        }
    }

    /// Creates an active execution component with no queued work.
    fn execution() -> Arc<SessionExecution> {
        Arc::new(
            SessionExecution::builder()
                .lifecycle(RwLock::new(SessionLifecycle::Active))
                .gate(AsyncMutex::new(()))
                .active_run(Mutex::new(None))
                .idle_notify(Notify::new())
                .cancellation(Mutex::new(CancellationToken::new()))
                .queue(Mutex::new(PendingQueue::default()))
                .sequence(std::sync::atomic::AtomicU64::new(0))
                .build(),
        )
    }

    /// Publishes and clears active Run correlation as one lifecycle value.
    #[tokio::test]
    async fn active_run_is_published_and_cleared_as_one_snapshot() {
        let execution = execution();
        let run_id = RunId::try_from("run-1").expect("run id");
        let turn_id = TurnId::try_from("turn-1").expect("turn id");
        let sink: Arc<dyn EventSink> = Arc::new(TestSink);
        let lease = execution
            .install_run(ActiveRunContext {
                run_id: run_id.clone(),
                turn_id: turn_id.clone(),
                sink: Arc::clone(&sink),
            })
            .expect("install run");

        let active = execution
            .active_run()
            .expect("active run snapshot")
            .expect("active run");
        assert_eq!(active.run_id, run_id);
        assert_eq!(active.turn_id, turn_id);
        assert!(Arc::ptr_eq(&active.sink, &sink));

        let waiting_execution = Arc::clone(&execution);
        let waiter = tokio::spawn(async move {
            waiting_execution
                .wait_for_idle()
                .await
                .expect("wait for idle")
        });
        tokio::task::yield_now().await;
        drop(lease);
        waiter.await.expect("idle waiter");
        assert!(execution.active_run().expect("active run").is_none());
    }

    /// The Run requires action from the first waiting prompt until the last
    /// settles, and a new Run never inherits a wait whose future was dropped.
    #[test]
    fn user_waits_report_only_the_first_and_the_last() {
        let execution = execution();
        assert!(execution.begin_user_wait());
        assert!(!execution.begin_user_wait());
        assert!(!execution.end_user_wait());
        assert!(execution.end_user_wait());
        assert!(!execution.end_user_wait());
        assert!(execution.begin_user_wait());
        let _lease = execution
            .install_run(ActiveRunContext {
                run_id: RunId::try_from("run-1").expect("run id"),
                turn_id: TurnId::try_from("turn-1").expect("turn id"),
                sink: Arc::new(TestSink),
            })
            .expect("install run");
        assert!(execution.begin_user_wait());
    }

    /// Rejects an operation that was queued before shutdown changed lifecycle state.
    #[tokio::test]
    async fn queued_operation_is_rejected_after_closing_starts() {
        let execution = execution();
        let guard = execution
            .acquire_operation()
            .await
            .expect("first operation");
        let waiting_execution = Arc::clone(&execution);
        let waiter = tokio::spawn(async move {
            match waiting_execution.acquire_operation().await {
                Ok(_guard) => Ok(()),
                Err(error) => Err(error),
            }
        });
        tokio::task::yield_now().await;

        let _closing = execution.begin_closing().expect("begin closing");
        drop(guard);

        assert!(matches!(
            waiter.await.expect("queued operation"),
            Err(KernelError::SessionClosing)
        ));
    }

    /// Close must wake queued operations before it waits for reporters holding those futures.
    #[tokio::test]
    async fn closing_rejects_waiters_without_releasing_gate() {
        let execution = execution();
        let guard = execution.acquire_operation().await.expect("gate");
        let queued = Arc::clone(&execution);
        let waiter = tokio::spawn(async move {
            queued.acquire_operation().await.map(|_| ())
        });
        tokio::task::yield_now().await;
        execution.request_close().expect("request close");
        let result =
            tokio::time::timeout(std::time::Duration::from_secs(1), waiter)
                .await
                .expect("waiter unblocked")
                .expect("join");
        assert!(matches!(result, Err(KernelError::SessionClosing)));
        drop(guard);
    }

    /// Cancelling while a prompt waits for the gate rejects it without installing a token.
    #[tokio::test]
    async fn cancel_before_admission_rejects_without_token() {
        let execution = execution();
        let guard = execution.acquire_operation().await.expect("gate");
        let previous = execution.cancellation().expect("token");
        let queued = Arc::clone(&execution);
        let waiter = tokio::spawn(async move {
            queued
                .acquire_operation_for_prompt(None, WaiterCancellation::Reject)
                .await
                .map(|(_guard, token)| token)
        });
        tokio::task::yield_now().await;
        execution.cancel().expect("cancel");
        drop(guard);

        let result = waiter.await.expect("join");
        assert!(matches!(result, Err(KernelError::PromptCancelled)));
        let current = execution.cancellation().expect("token");
        assert!(current.is_cancelled());
        assert!(previous.is_cancelled());
    }

    /// A cancel issued right after admission reaches the token installed during admission.
    #[tokio::test]
    async fn cancel_after_admission_cancels_installed_token() {
        let execution = execution();
        let (_guard, token) = execution
            .acquire_operation_for_prompt(None, WaiterCancellation::Reject)
            .await
            .expect("admitted");
        execution.cancel().expect("cancel");
        assert!(token.is_cancelled());
    }

    /// Internal waiters keep their place when a session-wide cancel only targets client prompts.
    #[tokio::test]
    async fn ignored_waiter_cancellation_still_admits() {
        let execution = execution();
        let guard = execution.acquire_operation().await.expect("gate");
        let queued = Arc::clone(&execution);
        let waiter = tokio::spawn(async move {
            queued
                .acquire_operation_for_prompt(None, WaiterCancellation::Ignore)
                .await
                .map(|(_guard, token)| token)
        });
        tokio::task::yield_now().await;
        execution.cancel().expect("cancel");
        drop(guard);

        let token = waiter.await.expect("join").expect("admitted");
        assert!(!token.is_cancelled());
    }

    /// A parent cancellation rejects a waiting internal submission.
    #[tokio::test]
    async fn parent_cancellation_rejects_ignored_waiter() {
        let execution = execution();
        let guard = execution.acquire_operation().await.expect("gate");
        let parent = CancellationToken::new();
        let queued = Arc::clone(&execution);
        let waiting_parent = parent.clone();
        let waiter = tokio::spawn(async move {
            queued
                .acquire_operation_for_prompt(
                    Some(&waiting_parent),
                    WaiterCancellation::Ignore,
                )
                .await
                .map(|_admission| ())
        });
        tokio::task::yield_now().await;
        parent.cancel();

        let result = waiter.await.expect("join");
        assert!(matches!(result, Err(KernelError::PromptCancelled)));
        drop(guard);
    }

    /// Restores an active lifecycle when shutdown fails before resource teardown.
    #[test]
    fn closing_can_be_aborted_before_teardown() {
        let execution = execution();

        let _closing = execution.begin_closing().expect("begin closing");
        execution.abort_closing().expect("abort closing");

        execution
            .ensure_active()
            .expect("execution restored active");
    }
}
