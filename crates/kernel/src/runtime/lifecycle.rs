use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};

use protocol::{PendingMessages, QueueId, RunId, Sequence, TurnId};
use tokio::sync::{Mutex as AsyncMutex, MutexGuard, Notify};
use tokio_util::sync::CancellationToken;

use super::{EventSink, KernelError, PendingQueue, PendingQueueItem, Session};

/// Serialized lifecycle state for one live session runtime.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SessionLifecycle {
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
pub(super) struct ActiveRunContext {
    pub(super) run_id: RunId,
    pub(super) turn_id: TurnId,
    pub(super) sink: Arc<dyn EventSink>,
}

/// Owns serialized operation, cancellation, queue, and event sequencing state.
#[derive(typed_builder::TypedBuilder)]
pub(super) struct SessionExecution {
    lifecycle: RwLock<SessionLifecycle>,
    gate: AsyncMutex<()>,
    #[builder(default)]
    active_run: Mutex<Option<ActiveRunContext>>,
    idle_notify: Notify,
    cancellation: Mutex<CancellationToken>,
    queue: Mutex<PendingQueue>,
    sequence: AtomicU64,
}

impl SessionExecution {
    /// Acquires the operation gate only while this execution remains active.
    pub(super) async fn acquire_operation(
        &self,
    ) -> Result<MutexGuard<'_, ()>, KernelError> {
        self.ensure_active()?;
        let guard = self.gate.lock().await;
        // Recheck after waiting because shutdown may have started while the
        // caller was queued behind an operation that already held the gate.
        self.ensure_active()?;
        Ok(guard)
    }

    /// Attempts to acquire the operation gate without queueing direct commands.
    pub(super) fn try_acquire_operation(
        &self,
    ) -> Result<Option<MutexGuard<'_, ()>>, KernelError> {
        self.ensure_active()?;
        let Ok(guard) = self.gate.try_lock() else {
            return Ok(None);
        };
        self.ensure_active()?;
        Ok(Some(guard))
    }

    /// Installs a fresh cancellation token for one serialized operation.
    pub(super) fn install_cancellation(
        &self,
    ) -> Result<CancellationToken, KernelError> {
        let cancellation = CancellationToken::new();
        *self
            .cancellation
            .lock()
            .map_err(|_poison_error| KernelError::Poisoned)? =
            cancellation.clone();
        Ok(cancellation)
    }

    /// Returns the current operation cancellation token without retaining its lock.
    pub(super) fn cancellation(
        &self,
    ) -> Result<CancellationToken, KernelError> {
        self.cancellation
            .lock()
            .map(|cancellation| cancellation.clone())
            .map_err(|_poison_error| KernelError::Poisoned)
    }

    /// Returns a defensive snapshot of all pending messages.
    pub(super) fn pending_snapshot(
        &self,
    ) -> Result<PendingMessages, KernelError> {
        self.queue
            .lock()
            .map(|queue| queue.snapshot())
            .map_err(|_poison_error| KernelError::Poisoned)
    }

    /// Returns pending identifiers in deterministic scheduling order.
    pub(super) fn pending_ids(&self) -> Result<Vec<QueueId>, KernelError> {
        self.queue
            .lock()
            .map(|queue| queue.ids())
            .map_err(|_poison_error| KernelError::Poisoned)
    }

    /// Reports whether any steering or follow-up message is pending.
    pub(super) fn has_pending(&self) -> Result<bool, KernelError> {
        self.queue
            .lock()
            .map(|queue| !queue.is_empty())
            .map_err(|_poison_error| KernelError::Poisoned)
    }

    /// Returns the next eligible pending item without consuming it.
    pub(super) fn next_pending(
        &self,
        allow_follow_up: bool,
    ) -> Result<Option<PendingQueueItem>, KernelError> {
        self.queue
            .lock()
            .map(|queue| queue.next(allow_follow_up))
            .map_err(|_poison_error| KernelError::Poisoned)
    }

    /// Adds one item after its enqueue record becomes durable.
    pub(super) fn push_pending(
        &self,
        item: PendingQueueItem,
    ) -> Result<(), KernelError> {
        self.queue
            .lock()
            .map_err(|_poison_error| KernelError::Poisoned)?
            .push(item);
        Ok(())
    }

    /// Removes one item after its cancellation or consumption is durable.
    pub(super) fn remove_pending(
        &self,
        queue_id: &QueueId,
    ) -> Result<(), KernelError> {
        self.queue
            .lock()
            .map_err(|_poison_error| KernelError::Poisoned)?
            .remove(queue_id);
        Ok(())
    }

    /// Removes a stable set of items under one queue lock after durable cancellation.
    pub(super) fn remove_pending_batch(
        &self,
        queue_ids: &[QueueId],
    ) -> Result<(), KernelError> {
        let mut queue = self
            .queue
            .lock()
            .map_err(|_poison_error| KernelError::Poisoned)?;
        for queue_id in queue_ids {
            queue.remove(queue_id);
        }
        Ok(())
    }

    /// Cancels the current serialized operation without waiting for settlement.
    pub(super) fn cancel(&self) -> Result<(), KernelError> {
        self.cancellation
            .lock()
            .map_err(|_poison_error| KernelError::Poisoned)?
            .cancel();
        Ok(())
    }

    /// Publishes all active-Run correlation fields under one lock.
    pub(super) fn install_run(
        self: &Arc<Self>,
        context: ActiveRunContext,
    ) -> Result<ActiveRunLease, KernelError> {
        let mut active_run = self
            .active_run
            .lock()
            .map_err(|_poison_error| KernelError::Poisoned)?;
        if active_run.is_some() {
            return Err(KernelError::Protocol(
                "session already has an active run".to_string(),
            ));
        }
        let run_id = context.run_id.clone();
        *active_run = Some(context);
        Ok(ActiveRunLease {
            execution: Arc::clone(self),
            run_id,
        })
    }

    /// Returns one owned active-Run snapshot without retaining the state lock.
    pub(super) fn active_run(
        &self,
    ) -> Result<Option<ActiveRunContext>, KernelError> {
        self.active_run
            .lock()
            .map(|active_run| active_run.clone())
            .map_err(|_poison_error| KernelError::Poisoned)
    }

    /// Advances the diagnostic Turn for the currently installed Run.
    pub(super) fn set_active_turn(
        &self,
        run_id: &RunId,
        turn_id: TurnId,
    ) -> Result<(), KernelError> {
        let mut active_run = self
            .active_run
            .lock()
            .map_err(|_poison_error| KernelError::Poisoned)?;
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
    pub(super) fn is_idle(&self) -> Result<bool, KernelError> {
        self.active_run().map(|active_run| active_run.is_none())
    }

    /// Waits until the current active Run lease has been released.
    pub(super) async fn wait_for_idle(&self) -> Result<(), KernelError> {
        loop {
            let notified = self.idle_notify.notified();
            if self.is_idle()? {
                return Ok(());
            }
            notified.await;
        }
    }

    /// Allocates the next positive event sequence for this Session.
    pub(super) fn next_sequence(&self) -> Result<Sequence, KernelError> {
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
    pub(super) fn begin_closing(&self) -> Result<(), KernelError> {
        let mut lifecycle = self
            .lifecycle
            .write()
            .map_err(|_poison_error| KernelError::Poisoned)?;
        match *lifecycle {
            SessionLifecycle::Starting => Err(KernelError::SessionStarting),
            SessionLifecycle::Active => {
                *lifecycle = SessionLifecycle::Closing;
                Ok(())
            }
            SessionLifecycle::Closing | SessionLifecycle::Closed => {
                Err(KernelError::SessionClosing)
            }
        }
    }

    /// Restores an active lifecycle when shutdown stops before resource teardown.
    pub(super) fn abort_closing(&self) -> Result<(), KernelError> {
        let mut lifecycle = self
            .lifecycle
            .write()
            .map_err(|_poison_error| KernelError::Poisoned)?;
        match *lifecycle {
            SessionLifecycle::Closing => {
                *lifecycle = SessionLifecycle::Active;
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
        *self
            .lifecycle
            .write()
            .map_err(|_poison_error| KernelError::Poisoned)? =
            SessionLifecycle::Closed;
        Ok(())
    }

    /// Reports whether teardown finished but retained resources still need cleanup.
    pub(super) fn is_closed(&self) -> Result<bool, KernelError> {
        self.lifecycle
            .read()
            .map(|lifecycle| matches!(*lifecycle, SessionLifecycle::Closed))
            .map_err(|_poison_error| KernelError::Poisoned)
    }

    /// Publishes a fully initialized execution as active after its startup checkpoint.
    pub(super) fn finish_starting(&self) -> Result<(), KernelError> {
        let mut lifecycle = self
            .lifecycle
            .write()
            .map_err(|_poison_error| KernelError::Poisoned)?;
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

    /// Rejects ordinary work before startup completes or after shutdown starts.
    pub(super) fn ensure_active(&self) -> Result<(), KernelError> {
        let lifecycle = self
            .lifecycle
            .read()
            .map_err(|_poison_error| KernelError::Poisoned)?;
        match *lifecycle {
            SessionLifecycle::Starting => Err(KernelError::SessionStarting),
            SessionLifecycle::Active => Ok(()),
            SessionLifecycle::Closing | SessionLifecycle::Closed => {
                Err(KernelError::SessionClosing)
            }
        }
    }
}

/// Clears active-Run correlation when one serialized Run exits any path.
pub(super) struct ActiveRunLease {
    execution: Arc<SessionExecution>,
    run_id: RunId,
}

impl Drop for ActiveRunLease {
    /// Clears only the active Run identity installed by this lease.
    fn drop(&mut self) {
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
    pub(super) fn sync_store(&self) -> Result<(), KernelError> {
        self.transcript.sync()
    }

    /// Acquires the run gate only while this runtime remains active.
    pub(super) async fn acquire_operation(
        &self,
    ) -> Result<MutexGuard<'_, ()>, KernelError> {
        self.execution.acquire_operation().await
    }

    /// Attempts to acquire the run gate without silently queueing direct commands.
    pub(super) fn try_acquire_operation(
        &self,
    ) -> Result<Option<MutexGuard<'_, ()>>, KernelError> {
        self.execution.try_acquire_operation()
    }

    /// Transitions an active runtime into closing while holding its run gate.
    pub(super) fn begin_closing(&self) -> Result<(), KernelError> {
        self.execution.begin_closing()
    }

    /// Restores an active runtime when shutdown stops before resource teardown.
    pub(super) fn abort_closing(&self) -> Result<(), KernelError> {
        self.execution.abort_closing()
    }

    /// Marks a closing runtime closed before its final map removal.
    pub(super) fn finish_closing(&self) -> Result<(), KernelError> {
        self.execution.finish_closing()
    }

    /// Reports whether this runtime is retained only for cleanup retry ownership.
    pub(super) fn is_closed(&self) -> Result<bool, KernelError> {
        self.execution.is_closed()
    }

    /// Publishes this runtime for ordinary operations after startup is durable.
    pub(super) fn finish_starting(&self) -> Result<(), KernelError> {
        self.execution.finish_starting()
    }

    /// Rejects ordinary kernel work before startup completes or after shutdown starts.
    pub(super) fn ensure_active(&self) -> Result<(), KernelError> {
        self.execution.ensure_active()
    }

    /// Preserves a typed extension-host error for run-gated shutdown reentry.
    pub(super) fn ensure_extension_operation(
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

        execution.begin_closing().expect("begin closing");
        drop(guard);

        assert!(matches!(
            waiter.await.expect("queued operation"),
            Err(KernelError::SessionClosing)
        ));
    }

    /// Restores an active lifecycle when shutdown fails before resource teardown.
    #[test]
    fn closing_can_be_aborted_before_teardown() {
        let execution = execution();

        execution.begin_closing().expect("begin closing");
        execution.abort_closing().expect("abort closing");

        execution
            .ensure_active()
            .expect("execution restored active");
    }

    /// A finalized lifecycle remains identifiable for resource-cleanup retries.
    #[test]
    fn closed_execution_is_identified_for_cleanup_retry() {
        let execution = execution();
        assert!(!execution.is_closed().expect("active lifecycle"));

        execution.begin_closing().expect("begin closing");
        execution.finish_closing().expect("finish closing");

        assert!(execution.is_closed().expect("closed lifecycle"));
    }
}
