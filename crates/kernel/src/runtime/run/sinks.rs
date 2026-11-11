//! Routing of Kernel-initiated Runs to the host's per-Session event projection.

use std::sync::Arc;

use protocol::SessionId;

use crate::EventSink;
use crate::runtime::{Kernel, KernelError};

impl Kernel {
    /// Installs one stable event router shared by all clones of this Kernel.
    pub fn install_event_sinks(
        &self,
        sinks: Arc<dyn crate::SessionEventSinks>,
    ) -> Result<(), KernelError> {
        self.event_sinks.set(sinks).map_err(|_lock_error| {
            tracing::warn!(
                "refused replacement of installed Session event sinks"
            );
            KernelError::EventSinksInstalled
        })
    }

    /// Uses the protocol adapter's projection or accepts events in headless operation.
    pub(in crate::runtime) fn session_sink(
        &self,
        session_id: &SessionId,
    ) -> Arc<dyn EventSink> {
        self.event_sinks.get().map_or_else(
            || Arc::new(crate::sink::DiscardEvents) as Arc<dyn EventSink>,
            |sinks| sinks.sink(session_id),
        )
    }
}
