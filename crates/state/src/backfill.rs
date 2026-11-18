//! One-time projection of every existing Session log (Codex `backfill_sessions`).
//!
//! A lease in `backfill_state` keeps concurrent processes from running it twice,
//! and a watermark lets an interrupted run resume. Re-projecting a file is
//! harmless because each database's cursor already covers applied lines.

mod lease;

use self::lease::{Claim, Lease};
use std::borrow::Cow;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use protocol::SessionId;
use store::{JsonlStoreFactory, LogHeader};
use tokio::io::AsyncBufReadExt;

use tokio_util::sync::{CancellationToken, DropGuard};

use crate::error::SqlText;

use crate::projector::{Position, SourceRevision};
use crate::{StateError, StateRuntime};

/// Lease length before another process may take over (Codex `BACKFILL_LEASE_SECONDS = 900`).
const LEASE_MS: u64 = 900_000;
/// Files processed between watermark and heartbeat writes.
const CHECKPOINT_EVERY: usize = 100;
/// Wait between lease checks (Codex `STARTUP_BACKFILL_POLL_INTERVAL`).
const LEASE_POLL: Duration = Duration::from_secs(1);

/// Result of one backfill call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackfillOutcome {
    /// An earlier run already finished.
    AlreadyComplete,
    /// Another process holds a live lease.
    LeaseHeld,
    /// This call ran to completion.
    Completed {
        /// Files after the watermark that were considered.
        scanned: usize,
        /// Files projected into both databases.
        projected: usize,
        /// Files that could not be read or projected.
        failed: usize,
    },
}

/// One backfill run over a sorted list of log paths.
struct Backfill<'a> {
    runtime: &'a StateRuntime,
    sessions_root: &'a Path,
    stop: &'a CancellationToken,
}

/// Identity and progress read in one scan of each database at startup.
#[derive(sqlx::FromRow)]
struct IndexedLog {
    id: String,
    #[sqlx(flatten)]
    position: Position,
}

/// Deferred listing keeps the filesystem off the peer lease polling path.
enum LogPaths<'a> {
    Supplied(&'a [PathBuf]),
    Store(&'a Arc<JsonlStoreFactory>),
}

impl<'a> LogPaths<'a> {
    /// Enumerates only after acquiring the lease or observing completion.
    async fn list(&self) -> Result<Cow<'a, [PathBuf]>, StateError> {
        match self {
            Self::Supplied(paths) => Ok(Cow::Borrowed(paths)),
            Self::Store(store) => {
                let store = Arc::clone(store);
                let paths =
                    tokio::task::spawn_blocking(move || store.session_paths())
                        .await
                        .map_err(std::io::Error::other)??;
                Ok(Cow::Owned(paths))
            }
        }
    }
}

impl Backfill<'_> {
    /// Drives startup through reconciliation, returning no outcome when shutdown interrupts it.
    async fn sync(
        &self,
        logs: LogPaths<'_>,
    ) -> Result<Option<BackfillOutcome>, StateError> {
        self.runtime
            .backfill_complete
            .store(false, Ordering::Release);
        let mut waiting = false;
        let claim = loop {
            if self.stop.is_cancelled() {
                return Ok(None);
            }
            match self.claim().await? {
                Claim::Held if matches!(logs, LogPaths::Store(_)) => {
                    if !waiting {
                        tracing::info!(
                            "waiting for another process to finish the state backfill"
                        );
                        waiting = true;
                    }
                    tokio::select! {
                        () = self.stop.cancelled() => return Ok(None),
                        () = tokio::time::sleep(LEASE_POLL) => {}
                    }
                }
                Claim::Held => return Ok(Some(BackfillOutcome::LeaseHeld)),
                claim => break claim,
            }
        };
        let (outcome, paths) = match claim {
            Claim::Owned(mut lease) => {
                let result: Result<_, StateError> = tokio::select! {
                    biased;
                    () = self.stop.cancelled() => Ok(None),
                    result = async {
                        let paths = logs.list().await?;
                        let outcome = self.project_all(&paths, &mut lease).await?;
                        Ok(Some((outcome, paths)))
                    } => result,
                };
                self.finish(&lease, matches!(&result, Ok(Some(_)))).await?;
                let Some(completed) = result? else {
                    return Ok(None);
                };
                completed
            }
            Claim::Complete => {
                (BackfillOutcome::AlreadyComplete, logs.list().await?)
            }
            Claim::Held => return Ok(Some(BackfillOutcome::LeaseHeld)),
        };
        tokio::select! {
            () = self.stop.cancelled() => return Ok(None),
            result = self.reconcile(&paths) => result?,
        }
        self.runtime
            .backfill_complete
            .store(true, Ordering::Release);
        Ok(Some(outcome))
    }

    /// Replays sorted logs while keeping the lease alive, even during a single long Session.
    async fn project_all(
        &self,
        paths: &[PathBuf],
        lease: &mut Lease,
    ) -> Result<BackfillOutcome, StateError> {
        tracing::info!(
            "started state backfill of {} session logs",
            paths.len()
        );
        let (mut scanned, mut projected, mut failed) = (0, 0, 0);
        let period = Duration::from_millis(LEASE_MS / 3);
        let mut heartbeat = tokio::time::interval_at(
            tokio::time::Instant::now() + period,
            period,
        );
        for path in paths {
            let Some(relative) = self.relative(path) else {
                tracing::warn!(
                    "skipped session log with a non-UTF-8 path {}",
                    path.display()
                );
                continue;
            };
            if lease
                .last_watermark
                .as_deref()
                .is_some_and(|mark| relative.as_str() <= mark)
            {
                continue;
            }
            scanned += 1;
            let project = self.project(path);
            tokio::pin!(project);
            let result = loop {
                tokio::select! {
                    result = &mut project => break result,
                    _ = heartbeat.tick() => self.checkpoint(lease).await?,
                }
            };
            match result {
                Ok(true) => projected += 1,
                Ok(false) => {}
                Err(error) => {
                    failed += 1;
                    tracing::warn!(
                        "failed to backfill session log {}: {}",
                        path.display(),
                        error
                    );
                }
            }
            lease.last_watermark = Some(relative);
            if scanned % CHECKPOINT_EVERY == 0 {
                self.checkpoint(lease).await?;
            }
        }
        tracing::info!(
            "scanned {} session logs for backfill: projected {}, failed {}",
            scanned,
            projected,
            failed
        );
        Ok(BackfillOutcome::Completed {
            scanned,
            projected,
            failed,
        })
    }

    /// Returns the watermark key of one path, or `None` when it is not UTF-8.
    fn relative(&self, path: &Path) -> Option<String> {
        path.strip_prefix(self.sessions_root)
            .unwrap_or(path)
            .to_str()
            .map(ToOwned::to_owned)
    }

    /// Projects one log unless its identity already belongs to another file.
    async fn project(&self, path: &Path) -> Result<bool, StateError> {
        let path = std::path::absolute(path)?;
        let path = path.as_path();
        let mut line = Vec::new();
        tokio::io::BufReader::new(tokio::fs::File::open(path).await?)
            .read_until(b'\n', &mut line)
            .await?;
        let header = LogHeader::try_from(line.trim_ascii_end())?;
        // Prefer the canonical copy while it exists; an offline rename transfers that identity.
        if let Some(existing) = self.runtime.session(&header.id).await?
            && existing.path.is_absolute()
            && existing.path != path
            && tokio::fs::try_exists(&existing.path).await?
            // The same home may have been opened through a symlink or a parent component.
            && tokio::fs::canonicalize(&existing.path).await? != tokio::fs::canonicalize(path).await?
        {
            return Ok(false);
        }
        self.runtime.refresh(&header.id, path).await?;
        Ok(true)
    }

    /// Repairs rows for logs that appeared, changed, or vanished without SQLite seeing it.
    ///
    /// Matching cursors in both databases need only a stat. Missing files are
    /// cleaned by path so a concurrent move keeps the newly projected rows.
    async fn reconcile(&self, paths: &[PathBuf]) -> Result<(), StateError> {
        let rows: Vec<IndexedLog> = sqlx::query_as(
            "SELECT s.id, s.path, COALESCE(p.next_byte_offset, 0) AS next_byte_offset, \
             COALESCE(p.next_sequence, 0) AS next_sequence, COALESCE(p.source_id, '') AS source_id, \
             COALESCE(p.source_size, 0) AS source_size, COALESCE(p.source_mtime, '') AS source_mtime FROM sessions s \
             LEFT JOIN projection_state p ON p.session_id = s.id",
        )
        .fetch_all(&self.runtime.state)
        .await?;
        let mut known: HashMap<_, _> = rows
            .into_iter()
            .map(|row| {
                (
                    row.position.path,
                    (row.id, row.position.cursor, row.position.source),
                )
            })
            .collect();
        let history: Vec<IndexedLog> = sqlx::query_as(
            "SELECT session_id AS id, path, next_byte_offset, next_sequence, source_id, source_size, source_mtime FROM projection_state",
        )
        .fetch_all(&self.runtime.history)
        .await?;
        let mut history: HashMap<_, _> = history
            .into_iter()
            .map(|row| (row.id, row.position))
            .collect();
        let (mut refreshed, mut failed) = (0, 0);
        let mut first_error = None;
        for path in paths {
            let path = std::path::absolute(path)?;
            let path = path.as_path();
            let text = path.sql_text()?;
            let metadata = match tokio::fs::metadata(path).await {
                Ok(metadata) => metadata,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    continue;
                }
                Err(error) => return Err(error.into()),
            };
            let result = match known.remove(text) {
                Some((id, cursor, source)) => {
                    let other = history.remove(&id);
                    if cursor.next_byte_offset == metadata.len()
                        && source == SourceRevision::try_from(&metadata)?
                        && other.is_some_and(|other| {
                            other.path == text
                                && other.cursor == cursor
                                && other.source == source
                        })
                    {
                        continue;
                    }
                    self.runtime.refresh(&SessionId::try_from(id)?, path).await
                }
                None => self.project(path).await.map(drop),
            };
            match result {
                Ok(()) => refreshed += 1,
                Err(error) => {
                    failed += 1;
                    tracing::warn!(
                        "failed to reconcile session log {}: {}",
                        path.display(),
                        error
                    );
                    // The store also skips non-v4 headers. A valid log whose projection
                    // failed must keep the catalog on its authoritative JSONL fallback.
                    if !matches!(
                        error,
                        StateError::Store(store::StoreError::InvalidSession(_))
                    ) {
                        first_error.get_or_insert(error);
                    }
                }
            }
        }
        let mut removed = 0;
        // A crash between the two commits can leave only the history half of a deletion.
        for (id, other) in history {
            known
                .entry(other.path)
                .or_insert((id, other.cursor, other.source));
        }
        for (path, (id, _, _)) in known {
            // A row outside the listing may still have a log elsewhere.
            if tokio::fs::try_exists(&path).await? {
                continue;
            }
            self.runtime
                .delete_session_at(
                    &SessionId::try_from(id)?,
                    Some(Path::new(&path)),
                )
                .await?;
            removed += 1;
        }
        if refreshed + failed + removed > 0 {
            tracing::info!(
                "reconciled state with session logs: refreshed {}, failed {}, removed {}",
                refreshed,
                failed,
                removed
            );
        }
        first_error.map_or(Ok(()), Err)
    }
}

/// Application-owned startup task. Dropping the handle also requests a checkpoint.
pub struct StartupBackfill {
    stop: DropGuard,
    task: tokio::task::JoinHandle<Result<(), StateError>>,
}

impl StartupBackfill {
    /// Starts catch-up after the application is fully constructed.
    pub fn spawn(
        runtime: Arc<StateRuntime>,
        store: Arc<JsonlStoreFactory>,
    ) -> Self {
        let cancel = CancellationToken::new();
        let stop = cancel.clone().drop_guard();
        let task = tokio::spawn(async move {
            let result = Backfill {
                runtime: &runtime,
                sessions_root: store.sessions_root(),
                stop: &cancel,
            }
            .sync(LogPaths::Store(&store))
            .await;
            if let Err(error) = &result {
                tracing::warn!(
                    "state sync with session logs failed; lookups keep scanning logs: {}",
                    error
                );
            }
            result.map(drop)
        });
        Self { stop, task }
    }

    /// Interrupts catch-up and waits until its owned lease has been released.
    pub async fn shutdown(self) -> Result<(), StateError> {
        drop(self.stop);
        self.task.await.map_err(std::io::Error::other)?
    }
}

impl StateRuntime {
    /// Polls a peer's lease, then reconciles this store before enabling indexed reads.
    pub async fn sync_logs(
        &self,
        store: &Arc<JsonlStoreFactory>,
    ) -> Result<(), StateError> {
        Backfill {
            runtime: self,
            sessions_root: store.sessions_root(),
            stop: &CancellationToken::new(),
        }
        .sync(LogPaths::Store(store))
        .await
        .map(drop)
    }

    /// Projects and reconciles supplied sorted paths, returning immediately on a live peer lease.
    pub async fn backfill(
        &self,
        sessions_root: &Path,
        paths: &[PathBuf],
    ) -> Result<BackfillOutcome, StateError> {
        Backfill {
            runtime: self,
            sessions_root,
            stop: &CancellationToken::new(),
        }
        .sync(LogPaths::Supplied(paths))
        .await?
        .ok_or_else(|| {
            std::io::Error::from(std::io::ErrorKind::Interrupted).into()
        })
    }
}
