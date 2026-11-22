//! Session metadata lookups served by SQLite when ready, else by scanning logs.
//!
//! JSONL stays authoritative: a missing, unready, or failing state runtime
//! never fails a lookup, it only routes it to the original `StoreFactory`
//! scans. Writes into SQLite are best effort for the same reason.

use std::collections::BTreeSet;
use std::path::Path;
use std::sync::Arc;

use protocol::{
    ContentHash, RequestContent, SessionId, SessionSummary, TrajectoryCursor,
    TrajectoryPage,
};
use state::{SessionFilter, StateError, StateRuntime};
use store::{SessionHeader, StoreFactory};
use tokio::sync::{Mutex, RwLock};
use tokio::time::{Duration, Instant};

use crate::runtime::KernelError;

/// Filesystem writes and stale-path repairs use the same failure tracking.
enum CatalogChange<'a> {
    Refresh(&'a Path),
    Delete(Option<&'a Path>),
}

/// Metadata lookups over the store and its optional SQLite mirror.
#[derive(Clone)]
pub(in crate::runtime) struct SessionCatalog {
    store_factory: Arc<dyn StoreFactory>,
    state: Option<Arc<StateRuntime>>,
    repairs: Arc<CatalogRepairs>,
}

/// Reads never wait for a repair's disk or SQLite I/O.
#[derive(Default)]
struct CatalogRepairs {
    status: Mutex<RepairStatus>,
    /// Order an identity repair against concurrent create, rename and delete writes.
    writers: RwLock<()>,
}

#[derive(Default)]
struct RepairStatus {
    dirty: BTreeSet<SessionId>,
    running: bool,
    retry_after: Option<Instant>,
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
            repairs: Arc::default(),
        }
    }

    /// Chooses the indexed read path, scheduling at most one eligible repair pass.
    fn state_for_read(&self) -> Option<&StateRuntime> {
        let state = self.state.as_ref().filter(|state| state.is_ready())?;
        let mut status = self.repairs.status.try_lock().ok()?;
        if status.dirty.is_empty() {
            return Some(state);
        }
        if !status.running
            && status.retry_after.is_none_or(|at| Instant::now() >= at)
        {
            status.running = true;
            self.spawn_repair(
                status.dirty.iter().cloned().collect(),
                Arc::clone(state),
            );
        }
        None
    }

    /// Attempts every pending identity before backing off persistent failures.
    fn spawn_repair(&self, ids: Vec<SessionId>, state: Arc<StateRuntime>) {
        let catalog = self.clone();
        tokio::spawn(async move {
            let mut failed = false;
            for id in ids {
                let _order = catalog.repairs.writers.write().await;
                match catalog.repair(&id, &state).await {
                    Ok(()) => {
                        catalog.repairs.status.lock().await.dirty.remove(&id);
                    }
                    Err(error) => {
                        failed = true;
                        tracing::warn!(
                            "could not repair session {} in SQLite: {}",
                            id,
                            error
                        );
                    }
                }
            }
            let mut status = catalog.repairs.status.lock().await;
            status.running = false;
            status.retry_after =
                failed.then(|| Instant::now() + Duration::from_secs(1));
            if status.dirty.is_empty() {
                tracing::info!(
                    "repaired session identities in SQLite; resuming indexed lookups"
                );
            }
        });
    }

    /// Resolves the current file off the async executor; it may have moved or been deleted.
    async fn repair(
        &self,
        id: &SessionId,
        state: &StateRuntime,
    ) -> Result<(), StateError> {
        let store = Arc::clone(&self.store_factory);
        let lookup = id.clone();
        let header = tokio::task::spawn_blocking(move || store.header(&lookup))
            .await
            .map_err(std::io::Error::other)??;
        match header {
            Some(header) => state.refresh(id, &header.path).await,
            None => state.delete_session(id).await,
        }
    }

    /// Reads one trajectory page; only the history database can answer.
    pub(in crate::runtime) async fn trajectory(
        &self,
        id: &SessionId,
        cursor: TrajectoryCursor,
        limit: Option<u32>,
    ) -> Result<TrajectoryPage, KernelError> {
        let state = self
            .state_for_read()
            .ok_or_else(|| KernelError::HistoryUnavailable(id.clone()))?;
        if let Some(page) = state.trajectory(id, cursor, limit).await? {
            return Ok(page);
        }
        // Not indexed yet: index it from its log, or learn that it is gone,
        // ordered against identity changes as background repairs are.
        {
            let _order = self.repairs.writers.write().await;
            self.repair(id, state).await?;
        }
        state
            .trajectory(id, cursor, limit)
            .await?
            .ok_or_else(|| KernelError::SessionNotFound(id.clone()))
    }

    /// Reads one request content by hash from the history database.
    pub(in crate::runtime) async fn request_content(
        &self,
        id: &SessionId,
        hash: &ContentHash,
    ) -> Result<Option<RequestContent>, KernelError> {
        let state = self
            .state_for_read()
            .ok_or_else(|| KernelError::HistoryUnavailable(id.clone()))?;
        if let Some(content) = state.request_content(id, hash).await? {
            return Ok(content);
        }
        // Not indexed yet: index it from its log, or learn that it is gone,
        // ordered against identity changes as background repairs are.
        {
            let _order = self.repairs.writers.write().await;
            self.repair(id, state).await?;
        }
        state
            .request_content(id, hash)
            .await?
            .ok_or_else(|| KernelError::SessionNotFound(id.clone()))
    }

    /// Lists Sessions newest first, optionally within one working directory.
    pub(in crate::runtime) async fn list(
        &self,
        cwd: Option<&Path>,
    ) -> Result<Vec<SessionSummary>, KernelError> {
        if let Some(state) = self.state_for_read() {
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
        if let Some(state) = self.state_for_read() {
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
        cwd: Option<&Path>,
    ) -> Result<Option<SessionHeader>, KernelError> {
        if let Some(state) = self.state_for_read() {
            match state.session(id).await {
                Ok(Some(row)) => match tokio::fs::try_exists(&row.path).await {
                    Ok(true) => {
                        return Ok(cwd
                            .is_none_or(|cwd| cwd == row.cwd)
                            .then(|| row.into()));
                    }
                    Ok(false) if cwd.is_some_and(|cwd| cwd != row.cwd) => {
                        // A scoped fallback may find a moved log; it may not evict a different workspace.
                    }
                    Ok(false) => {
                        tracing::warn!(
                            "session {} log {} is gone; removing its state row",
                            id,
                            row.path.display()
                        );
                        self.update(id, CatalogChange::Delete(Some(&row.path)))
                            .await;
                    }
                    Err(error) => tracing::warn!(
                        "could not check session {} log {}; keeping its index and scanning logs: {}",
                        id,
                        row.path.display(),
                        error
                    ),
                },
                Ok(None) => {}
                Err(error) => tracing::warn!(
                    "state lookup of session {} failed; scanning session logs: {}",
                    id,
                    error
                ),
            }
        }
        let header = self
            .store_factory
            .header(id)?
            .filter(|header| cwd.is_none_or(|cwd| header.cwd == cwd));
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
        Ok(self.header(id, None).await?.is_some())
    }

    /// Lists the Sessions owned by `owner`, created in its `cwd`.
    pub(in crate::runtime) async fn children(
        &self,
        owner: &SessionId,
        cwd: &Path,
    ) -> Result<Vec<SessionHeader>, KernelError> {
        if let Some(state) = self.state_for_read() {
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
        self.state.as_ref().is_some_and(|state| state.is_ready())
            && self
                .repairs
                .status
                .try_lock()
                .is_ok_and(|status| status.dirty.is_empty())
    }

    /// Projects one Session's log; failures are logged and repaired later.
    pub(in crate::runtime) async fn refresh(
        &self,
        id: &SessionId,
        path: &Path,
    ) {
        self.update(id, CatalogChange::Refresh(path)).await;
    }

    /// Drops one deleted Session from SQLite; failures are logged.
    pub(in crate::runtime) async fn record_deleted(&self, id: &SessionId) {
        self.update(id, CatalogChange::Delete(None)).await;
    }

    /// Orders identity changes against repair and immediately degrades the catalog on failure.
    async fn update(&self, id: &SessionId, change: CatalogChange<'_>) {
        let Some(state) = &self.state else { return };
        // Healthy writers can overlap. A repair must finish before a newer change publishes.
        let _order = self.repairs.writers.read().await;
        let result = match change {
            CatalogChange::Refresh(path) => state.refresh(id, path).await,
            CatalogChange::Delete(path) => {
                state.delete_session_at(id, path).await
            }
        };
        if let Err(error) = result {
            self.repairs.status.lock().await.dirty.insert(id.clone());
            tracing::warn!(
                "failed to update session {} in SQLite; using session logs until repaired: {}",
                id,
                error
            );
        }
    }
}
