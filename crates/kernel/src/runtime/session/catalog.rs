//! Session metadata lookups served by SQLite when ready, else by scanning logs.
//!
//! JSONL stays authoritative: a missing, unready, or failing state runtime
//! never fails a lookup, it only routes it to the original `StoreFactory`
//! scans. Writes into SQLite are best effort for the same reason.

use std::path::Path;
use std::sync::Arc;

use protocol::{SessionId, SessionSummary};
use state::{SessionFilter, StateRuntime};
use store::{SessionHeader, StoreFactory};

use crate::runtime::KernelError;

/// Metadata lookups over the store and its optional SQLite mirror.
#[derive(Clone)]
pub(in crate::runtime) struct SessionCatalog {
    store_factory: Arc<dyn StoreFactory>,
    state: Option<Arc<StateRuntime>>,
}

impl SessionCatalog {
    /// Pairs the authoritative store with its optional SQLite mirror.
    pub(in crate::runtime) fn new(
        store_factory: Arc<dyn StoreFactory>,
        state: Option<Arc<StateRuntime>>,
    ) -> Self {
        Self {
            store_factory,
            state,
        }
    }

    /// Returns the mirror only once its backfill made SQLite complete.
    fn ready(&self) -> Option<&StateRuntime> {
        self.state.as_deref().filter(|state| state.is_ready())
    }

    /// Lists Sessions newest first, optionally within one working directory.
    pub(in crate::runtime) async fn list(
        &self,
        cwd: Option<&Path>,
    ) -> Result<Vec<SessionSummary>, KernelError> {
        if let Some(state) = self.ready() {
            match state
                .list_sessions(SessionFilter::builder().cwd(cwd).build())
                .await
            {
                Ok(page) => {
                    return Ok(page
                        .sessions
                        .into_iter()
                        .map(SessionSummary::from)
                        .collect());
                }
                Err(error) => tracing::warn!(
                    "state listing failed; scanning session logs: {}",
                    error
                ),
            }
        }
        Ok(self
            .store_factory
            .list(cwd)?
            .into_iter()
            .map(SessionSummary::from)
            .collect())
    }

    /// Returns one Session's listing summary.
    pub(in crate::runtime) async fn summary(
        &self,
        id: &SessionId,
    ) -> Result<Option<SessionSummary>, KernelError> {
        if let Some(state) = self.ready() {
            match state.session(id).await {
                Ok(row) => return Ok(row.map(SessionSummary::from)),
                Err(error) => tracing::warn!(
                    "state lookup of session {} failed; scanning session logs: {}",
                    id,
                    error
                ),
            }
        }
        Ok(self
            .store_factory
            .list(None)?
            .into_iter()
            .map(SessionSummary::from)
            .find(|summary| &summary.session_id == id))
    }

    /// Locates one Session's log, repairing SQLite when it disagrees with disk.
    ///
    /// A row whose file vanished is deleted before falling back to the scan,
    /// and a log the scan finds but SQLite lacks is projected on the way out.
    pub(in crate::runtime) async fn header(
        &self,
        id: &SessionId,
    ) -> Result<Option<SessionHeader>, KernelError> {
        if let Some(state) = self.ready() {
            match state.session(id).await {
                Ok(Some(row))
                    if tokio::fs::try_exists(&row.path)
                        .await
                        .unwrap_or(false) =>
                {
                    return Ok(Some(row.into()));
                }
                Ok(Some(row)) => {
                    tracing::warn!(
                        "session {} log {} is gone; removing its state row",
                        id,
                        row.path.display()
                    );
                    self.record_deleted(id).await;
                }
                Ok(None) => {}
                Err(error) => tracing::warn!(
                    "state lookup of session {} failed; scanning session logs: {}",
                    id,
                    error
                ),
            }
        }
        let header = self.store_factory.header(id)?;
        if let Some(header) = &header {
            self.refresh(id, &header.path).await;
        }
        Ok(header)
    }

    /// Reports whether a persisted log exists for `id`.
    pub(in crate::runtime) async fn exists(
        &self,
        id: &SessionId,
    ) -> Result<bool, KernelError> {
        Ok(self.header(id).await?.is_some())
    }

    /// Lists the Sessions owned by `owner`, created in its `cwd`.
    pub(in crate::runtime) async fn children(
        &self,
        owner: &SessionId,
        cwd: &Path,
    ) -> Result<Vec<SessionHeader>, KernelError> {
        if let Some(state) = self.ready() {
            match state.children(owner).await {
                Ok(rows) => {
                    return Ok(rows
                        .into_iter()
                        .map(SessionHeader::from)
                        .collect());
                }
                Err(error) => tracing::warn!(
                    "state children of session {} failed; scanning session logs: {}",
                    owner,
                    error
                ),
            }
        }
        self.scan_children(owner, cwd)
    }

    /// Scans the owner's cwd headers for its children; the synchronous fallback.
    pub(in crate::runtime) fn scan_children(
        &self,
        owner: &SessionId,
        cwd: &Path,
    ) -> Result<Vec<SessionHeader>, KernelError> {
        Ok(self
            .store_factory
            .headers(Some(cwd))?
            .into_iter()
            .filter(|header| {
                header
                    .ownership
                    .as_ref()
                    .is_some_and(|edge| &edge.session_id == owner)
            })
            .collect())
    }

    /// Reports whether SQLite can answer children lookups right now.
    pub(in crate::runtime) fn is_ready(&self) -> bool {
        self.ready().is_some()
    }

    /// Projects one Session's log; failures are logged and repaired later.
    pub(in crate::runtime) async fn refresh(
        &self,
        id: &SessionId,
        path: &Path,
    ) {
        // Writes happen before the backfill finishes too, so they are never lost.
        if let Some(state) = &self.state
            && let Err(error) = state.refresh(id, path).await
        {
            tracing::warn!(
                "failed to project session {} into state: {}",
                id,
                error
            );
        }
    }

    /// Drops one deleted Session from SQLite; failures are logged.
    pub(in crate::runtime) async fn record_deleted(&self, id: &SessionId) {
        if let Some(state) = &self.state
            && let Err(error) = state.delete_session(id).await
        {
            tracing::warn!(
                "failed to remove session {} from state: {}",
                id,
                error
            );
        }
    }
}
