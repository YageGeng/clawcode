//! Atomic ownership and checkpoint updates for the shared startup backfill.
use super::{Backfill, LEASE_MS};
use crate::StateError;
use store::{Clock, SystemClock};

/// Only the current owner can checkpoint, release or complete this lease.
#[derive(sqlx::FromRow)]
pub(super) struct Lease {
    lease_owner: String,
    pub(super) last_watermark: Option<String>,
}

/// A completed database can reconcile immediately; a peer must wait for the owner.
pub(super) enum Claim {
    Complete,
    Held,
    Owned(Lease),
}

impl Backfill<'_> {
    /// Reads the common wait/complete cases without taking a writer lock.
    pub(super) async fn claim(&self) -> Result<Claim, StateError> {
        let (status, updated): (String, i64) = sqlx::query_as(
            "SELECT status, updated_at_ms FROM backfill_state WHERE id = 1",
        )
        .fetch_one(&self.runtime.state)
        .await?;
        let now = SystemClock.now().get();
        let expiry = i64::try_from(now.saturating_sub(LEASE_MS))?;
        if status == "complete" {
            return Ok(Claim::Complete);
        }
        if status == "running" && updated >= expiry {
            return Ok(Claim::Held);
        }
        // The winning UPDATE returns its own watermark even if a peer changed it after the read.
        let lease = sqlx::query_as(
            "UPDATE backfill_state SET status = 'running', updated_at_ms = ?, lease_owner = lower(hex(randomblob(16))) \
             WHERE id = 1 AND (status = 'pending' OR (status = 'running' AND updated_at_ms < ?)) \
             RETURNING lease_owner, last_watermark",
        )
        .bind(i64::try_from(now)?)
        .bind(expiry)
        .fetch_optional(&self.runtime.state)
        .await?;
        Ok(lease.map_or(Claim::Held, Claim::Owned))
    }

    /// Releases only the owned lease and saves the last fully considered file.
    pub(super) async fn finish(
        &self,
        lease: &Lease,
        complete: bool,
    ) -> Result<(), StateError> {
        let now = i64::try_from(SystemClock.now().get())?;
        let status = if complete { "complete" } else { "pending" };
        let changed = sqlx::query(
            "UPDATE backfill_state SET status = ?, last_watermark = ?, updated_at_ms = ?, \
             last_success_at_ms = CASE WHEN ? THEN ? ELSE last_success_at_ms END, lease_owner = NULL \
             WHERE id = 1 AND lease_owner = ?",
        )
        .bind(status)
        .bind(&lease.last_watermark)
        .bind(now)
        .bind(complete)
        .bind(now)
            .bind(&lease.lease_owner)
            .execute(&self.runtime.state)
            .await?.rows_affected();
        if changed == 0 {
            return Err(StateError::LeaseLost);
        }
        tracing::info!("released state backfill lease with status {}", status);
        Ok(())
    }

    /// Advances the retained watermark only while this process still owns the lease.
    pub(super) async fn checkpoint(
        &self,
        lease: &Lease,
    ) -> Result<(), StateError> {
        let changed = sqlx::query("UPDATE backfill_state SET last_watermark = ?, updated_at_ms = ? WHERE id = 1 AND lease_owner = ?")
            .bind(&lease.last_watermark)
            .bind(i64::try_from(SystemClock.now().get())?)
            .bind(&lease.lease_owner)
            .execute(&self.runtime.state)
            .await?.rows_affected();
        if changed == 0 {
            return Err(StateError::LeaseLost);
        }
        tracing::debug!(
            "checkpointed state backfill at {:?}",
            lease.last_watermark
        );
        Ok(())
    }
}
