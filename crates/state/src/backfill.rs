//! One-time projection of every existing Session log (Codex `backfill_sessions`).
//!
//! A lease in `backfill_state` keeps concurrent processes from running it twice,
//! and a watermark lets an interrupted run resume. Re-projecting a file is
//! harmless because each database's cursor already covers applied lines.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use protocol::SessionId;
use store::{Clock, JsonlStoreFactory, LogHeader, SystemClock};
use tokio::io::AsyncBufReadExt;

use crate::error::SqlText;
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
    now_ms: u64,
}

impl Backfill<'_> {
    /// Claims the lease, projects every file past the watermark, and marks completion.
    async fn run(
        &self,
        paths: &[PathBuf],
    ) -> Result<BackfillOutcome, StateError> {
        let (status, watermark): (String, Option<String>) = sqlx::query_as(
            "SELECT status, last_watermark FROM backfill_state WHERE id = 1",
        )
        .fetch_one(&self.runtime.state)
        .await?;
        if status == "complete" {
            self.runtime
                .backfill_complete
                .store(true, Ordering::Release);
            return Ok(BackfillOutcome::AlreadyComplete);
        }
        let claimed = sqlx::query(
            "UPDATE backfill_state SET status = 'running', updated_at_ms = ? WHERE id = 1 \
             AND (status = 'pending' OR (status = 'running' AND updated_at_ms < ?))",
        )
        .bind(i64::try_from(self.now_ms)?)
        .bind(i64::try_from(self.now_ms.saturating_sub(LEASE_MS))?)
        .execute(&self.runtime.state)
        .await?
        .rows_affected();
        if claimed != 1 {
            return Ok(BackfillOutcome::LeaseHeld);
        }
        tracing::info!(
            "started state backfill of {} session logs",
            paths.len()
        );
        let (mut scanned, mut projected, mut failed) = (0, 0, 0);
        let mut last = watermark.clone();
        for path in paths {
            let Some(relative) = self.relative(path) else {
                tracing::warn!(
                    "skipped session log with a non-UTF-8 path {}",
                    path.display()
                );
                continue;
            };
            if watermark
                .as_deref()
                .is_some_and(|mark| relative.as_str() <= mark)
            {
                continue;
            }
            scanned += 1;
            match self.project(path).await {
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
            last = Some(relative);
            if scanned % CHECKPOINT_EVERY == 0 {
                self.checkpoint(last.as_deref()).await?;
            }
        }
        sqlx::query(
            "UPDATE backfill_state SET status = 'complete', last_watermark = ?, \
             last_success_at_ms = ?, updated_at_ms = ? WHERE id = 1",
        )
        .bind(last.as_deref())
        .bind(i64::try_from(self.now_ms)?)
        .bind(i64::try_from(self.now_ms)?)
        .execute(&self.runtime.state)
        .await?;
        self.runtime
            .backfill_complete
            .store(true, Ordering::Release);
        tracing::info!(
            "completed state backfill: scanned {}, projected {}, failed {}",
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
        let mut line = Vec::new();
        tokio::io::BufReader::new(tokio::fs::File::open(path).await?)
            .read_until(b'\n', &mut line)
            .await?;
        let header = LogHeader::try_from(line.trim_ascii_end())?;
        // Codex rule: never move a Session to another copy of its log.
        if let Some(existing) = self.runtime.session(&header.id).await?
            && existing.path != path
        {
            return Ok(false);
        }
        self.runtime.refresh(&header.id, path).await?;
        Ok(true)
    }

    /// Repairs rows for logs that appeared, changed, or vanished without SQLite seeing it.
    ///
    /// Only stats each file: a log whose length differs from its cursor is
    /// refreshed (the projector resets a shrunk or rewritten one), an unknown
    /// log is projected, and a row whose log is gone is removed.
    async fn reconcile(&self, paths: &[PathBuf]) -> Result<(), StateError> {
        let rows: Vec<(String, String, u64)> = sqlx::query_as(
            "SELECT s.id, s.path, COALESCE(p.next_byte_offset, 0) FROM sessions s \
             LEFT JOIN projection_state p ON p.session_id = s.id",
        )
        .fetch_all(&self.runtime.state)
        .await?;
        let mut known: HashMap<String, (String, u64)> = rows
            .into_iter()
            .map(|(id, path, cursor)| (path, (id, cursor)))
            .collect();
        let (mut refreshed, mut failed) = (0, 0);
        for path in paths {
            let (Ok(text), Ok(metadata)) =
                (path.sql_text(), tokio::fs::metadata(path).await)
            else {
                continue;
            };
            let result = match known.remove(text) {
                Some((_, cursor)) if cursor == metadata.len() => continue,
                Some((id, _)) => match SessionId::try_from(id) {
                    Ok(id) => self.runtime.refresh(&id, path).await,
                    Err(error) => Err(error.into()),
                },
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
                }
            }
        }
        let mut removed = 0;
        for (path, (id, _)) in known {
            // A row outside the listing may still have a log elsewhere.
            if tokio::fs::try_exists(&path).await.unwrap_or(true) {
                continue;
            }
            self.runtime
                .delete_session(&SessionId::try_from(id)?)
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
        Ok(())
    }

    /// Records progress so an interrupted run resumes after `watermark`.
    async fn checkpoint(
        &self,
        watermark: Option<&str>,
    ) -> Result<(), StateError> {
        let now = SystemClock.now().get();
        sqlx::query("UPDATE backfill_state SET last_watermark = ?, updated_at_ms = ? WHERE id = 1")
            .bind(watermark)
            .bind(i64::try_from(now)?)
            .execute(&self.runtime.state)
            .await?;
        tracing::info!(
            "checkpointed state backfill at {}",
            watermark.unwrap_or_default()
        );
        Ok(())
    }
}

impl StateRuntime {
    /// Brings SQLite up to every log under the store's root at startup.
    ///
    /// Runs the one-time backfill, polling while another process holds its
    /// lease, then reconciles rows that drifted while SQLite was not told
    /// (an older binary, a failed init, a lost refresh).
    pub async fn sync_logs(
        &self,
        store: &Arc<JsonlStoreFactory>,
    ) -> Result<(), StateError> {
        let mut waiting = false;
        loop {
            let lister = Arc::clone(store);
            let paths =
                tokio::task::spawn_blocking(move || lister.session_paths())
                    .await
                    .map_err(std::io::Error::other)??;
            let backfill = Backfill {
                runtime: self,
                sessions_root: store.sessions_root(),
                now_ms: SystemClock.now().get(),
            };
            match backfill.run(&paths).await? {
                BackfillOutcome::LeaseHeld => {
                    if !waiting {
                        tracing::info!(
                            "waiting for another process to finish the state backfill"
                        );
                        waiting = true;
                    }
                    tokio::time::sleep(LEASE_POLL).await;
                }
                _ => return backfill.reconcile(&paths).await,
            }
        }
    }

    /// Projects every existing log once; `paths` must be sorted.
    pub async fn backfill(
        &self,
        sessions_root: &Path,
        paths: &[PathBuf],
    ) -> Result<BackfillOutcome, StateError> {
        let now_ms = SystemClock.now().get();
        Backfill {
            runtime: self,
            sessions_root,
            now_ms,
        }
        .run(paths)
        .await
    }
}
