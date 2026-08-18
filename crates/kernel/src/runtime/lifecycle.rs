use tokio::sync::MutexGuard;

use super::{KernelError, SessionRuntime};

/// Serialized lifecycle state for one live session runtime.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SessionLifecycle {
    /// The session accepts normal agent and maintenance operations.
    Active,
    /// Shutdown hooks are running and no new serialized work may start.
    Closing,
    /// Shutdown completed and the runtime is no longer registered.
    Closed,
}

impl SessionRuntime {
    /// Forces all buffered session mutations to durable storage.
    pub(super) fn sync_store(&self) -> Result<(), KernelError> {
        let mut store = self
            .store
            .lock()
            .map_err(|_poison_error| KernelError::Poisoned)?;
        if let Err(error) = store.sync() {
            tracing::error!(
                "failed to sync persistent state for session {}: {}",
                store.session_id(),
                error
            );
            return Err(error.into());
        }
        Ok(())
    }

    /// Acquires the run gate only while this runtime remains active.
    pub(super) async fn acquire_operation(
        &self,
    ) -> Result<MutexGuard<'_, ()>, KernelError> {
        self.ensure_active()?;
        let guard = self.run_gate.lock().await;
        self.ensure_active()?;
        Ok(guard)
    }

    /// Attempts to acquire the run gate without silently queueing direct commands.
    pub(super) fn try_acquire_operation(
        &self,
    ) -> Result<Option<MutexGuard<'_, ()>>, KernelError> {
        self.ensure_active()?;
        let Ok(guard) = self.run_gate.try_lock() else {
            return Ok(None);
        };
        self.ensure_active()?;
        Ok(Some(guard))
    }

    /// Transitions an active runtime into closing while holding its run gate.
    pub(super) fn begin_closing(&self) -> Result<(), KernelError> {
        let mut lifecycle = self
            .lifecycle
            .write()
            .map_err(|_poison_error| KernelError::Poisoned)?;
        match *lifecycle {
            SessionLifecycle::Active => {
                *lifecycle = SessionLifecycle::Closing;
                Ok(())
            }
            SessionLifecycle::Closing | SessionLifecycle::Closed => {
                Err(KernelError::SessionClosing)
            }
        }
    }

    /// Marks a closing runtime closed before its final map removal.
    pub(super) fn finish_closing(&self) -> Result<(), KernelError> {
        *self
            .lifecycle
            .write()
            .map_err(|_poison_error| KernelError::Poisoned)? =
            SessionLifecycle::Closed;
        Ok(())
    }

    /// Rejects ordinary kernel work once shutdown has started.
    pub(super) fn ensure_active(&self) -> Result<(), KernelError> {
        let lifecycle = self
            .lifecycle
            .read()
            .map_err(|_poison_error| KernelError::Poisoned)?;
        match *lifecycle {
            SessionLifecycle::Active => Ok(()),
            SessionLifecycle::Closing | SessionLifecycle::Closed => {
                Err(KernelError::SessionClosing)
            }
        }
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
